//! Code quotes driven through the `vyrn` binary. Generation runs
//! with the cache disabled so a stale entry never masks a change.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

fn vyrn() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_vyrn"));
    c.env("VYRN_NO_GEN_CACHE", "1");
    c
}

static COUNTER: AtomicUsize = AtomicUsize::new(0);

fn scratch(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("vyrn_cq_{tag}_{}_{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write(path: &Path, body: &str) {
    std::fs::write(path, body).unwrap();
}

/// Writes a generator and an app to a fresh directory; returns the app path.
fn gen_app(tag: &str, gen: &str, app: &str) -> PathBuf {
    let dir = scratch(tag);
    write(&dir.join("gen.vyrn"), gen);
    write(&dir.join("app.vyrn"), app);
    dir.join("app.vyrn")
}

#[test]
fn emit_gen_emits_escaped_and_spliced_source() {
    let gen = "export gen fn mkMod(who: String) -> Code {\n\
               let greeting = \"hi, \"\n\
               let body = vyrn\"\"\"export fn greet\\{name(who)}(who: String) -> String {\n\
               return \\{greeting} + who\n\
               }\n\"\"\"\n\
               return body\n\
               }\n";
    let app = "import { mkMod } from \"./gen\"\n\
               import { greetBob } from mkMod(\"Bob\")\n\
               fn main() -> Int64 { print(greetBob(\"x\")) return 0 }\n";
    let app_path = gen_app("emit", gen, app);
    let out = vyrn().arg("emit-gen").arg(&app_path).output().unwrap();
    assert!(
        out.status.success(),
        "emit-gen failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let src = String::from_utf8_lossy(&out.stdout);
    assert!(
        src.contains("export fn greetBob(who: String)"),
        "fragment splice:\n{src}"
    );
    assert!(
        src.contains("return \"hi, \" + who"),
        "string→literal splice:\n{src}"
    );
}

#[test]
fn a_generator_returning_code_is_an_import_target() {
    let gen = "export gen fn make() -> Code {\n\
               return vyrn\"export fn made() -> Int64 { return 7 }\"\n\
               }\n";
    let app = "import { make } from \"./gen\"\n\
               import { made } from make()\n\
               fn main() -> Int64 { return made() }\n";
    let out = vyrn()
        .arg("run")
        .arg(gen_app("code", gen, app))
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(7),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A namespace import takes a `Name` hole for its alias and a `String` hole for
/// its specifier in one quote: only the hole after `from` is a string literal
/// in the skeleton.
#[test]
fn a_namespace_import_splices_its_alias_and_its_specifier() {
    let dir = scratch("nsimport");
    write(
        &dir.join("lib.vyrn"),
        "export fn seven() -> Int64 { return 7 }\n",
    );
    write(
        &dir.join("gen.vyrn"),
        "export gen fn make(spec: String) -> Code {\n\
         \x20   return vyrn\"\"\"\n\
         \x20       import * as \\{name(\"lib\")} from \\{spec}\n\
         \x20       export fn made() -> Int64 { return lib.seven() }\n\
         \x20       \"\"\"\n\
         }\n",
    );
    write(
        &dir.join("app.vyrn"),
        "import { make } from \"./gen\"\n\
         import { made } from make(\"./lib\")\n\
         fn main() -> Int64 { return made() }\n",
    );
    let out = vyrn()
        .arg("run")
        .arg(dir.join("app.vyrn"))
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(7),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn injection_attempt_becomes_an_inert_string_literal() {
    let gen = "export gen fn mkMod(fname: String) -> Code {\n\
               let evil = \"\\\"; dropTables(); \\\"\"\n\
               let body = vyrn\"\"\"export fn \\{name(fname)}() -> String { return \\{evil} }\"\"\"\n\
               return body\n\
               }\n";
    let app = "import { mkMod } from \"./gen\"\n\
               import { f } from mkMod(\"f\")\n\
               fn main() -> Int64 { print(f()) return 0 }\n";
    let app_path = gen_app("inj", gen, app);
    let out = vyrn().arg("run").arg(&app_path).output().unwrap();
    assert!(
        out.status.success(),
        "run failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("; dropTables(); "),
        "payload printed as data:\n{stdout}"
    );
}

#[test]
fn broken_skeleton_reports_in_the_generators_file() {
    // `type Query {` is GraphQL, not Vyrn, so the skeleton does not parse.
    let gen = "export gen fn mkMod(name: String) -> Code {\n\
               let body = vyrn\"\"\"\n\
               type Query {\n\
               }\n\"\"\"\n\
               return body\n\
               }\n";
    let app = "import { mkMod } from \"./gen\"\n\
               import { x } from mkMod(\"x\")\n\
               fn main() -> Int64 { return 0 }\n";
    let app_path = gen_app("skel", gen, app);
    let out = vyrn().arg("check").arg(&app_path).output().unwrap();
    assert!(!out.status.success(), "expected a skeleton error");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("gen.vyrn"),
        "reported in the generator's file:\n{err}"
    );
    assert!(
        err.contains("skeleton does not parse"),
        "skeleton message:\n{err}"
    );
}

#[test]
fn bad_identifier_splice_names_the_generator() {
    let gen = "export gen fn mkMod(name: String) -> Code {\n\
               let body = vyrn\"\"\"export fn \\{name}() -> Int64 { return 0 }\"\"\"\n\
               return body\n\
               }\n";
    let app = "import { mkMod } from \"./gen\"\n\
               import { f } from mkMod(\"a b\")\n\
               fn main() -> Int64 { return 0 }\n";
    let app_path = gen_app("ident", gen, app);
    let out = vyrn().arg("check").arg(&app_path).output().unwrap();
    assert!(!out.status.success(), "expected an identifier-splice error");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("mkMod"), "names the generator:\n{err}");
    assert!(
        err.contains("\"a b\""),
        "quotes the offending value:\n{err}"
    );
}

/// `helper` is exported only to `./gen`, whose quotes import it: `make` writes
/// the import in a quote, `copy` copies it from an input with `codeAt`.
fn authors_app(tag: &str, app: &str) -> std::process::Output {
    let dir = scratch(tag);
    write(
        &dir.join("lib.vyrn"),
        "export to \"./gen\" fn helper() -> Int64 { return 7 }\n",
    );
    write(
        &dir.join("script.txt"),
        "import { helper } from \"./lib\"\nexport fn made() -> Int64 { return helper() }\n",
    );
    let gen = "export gen fn make() -> Code {\n\
               return vyrn\"\"\"import { helper } from \"./lib\"\n\
               export fn made() -> Int64 { return helper() }\n\
               \"\"\"\n\
               }\n\
               export gen fn copy(path: String) -> Code {\n\
               let text = match readFile(path) {\n\
               Ok(t) => t,\n\
               Err(e) => panic(e),\n\
               }\n\
               return codeAt(path, 1, 1, text)\n\
               }\n";
    write(&dir.join("gen.vyrn"), gen);
    write(
        &dir.join("wrap.vyrn"),
        "import { make } from \"./gen\"\n\
               export gen fn wrap() -> Code {\n\
               return vyrn\"\"\"// wrapped\n\\{make()}\"\"\"\n\
               }\n",
    );
    write(&dir.join("stray.vyrn"), "export gen fn stray() -> Code {\n\
               return vyrn\"import { helper } from \\\"./lib\\\"\\nexport fn made() -> Int64 { return helper() }\"\n\
               }\n");
    write(&dir.join("app.vyrn"), app);
    vyrn()
        .arg("run")
        .arg(dir.join("app.vyrn"))
        .output()
        .unwrap()
}

/// A quote in a module `export to` does not name is refused at the quote's
/// module and line.
#[test]
fn an_import_a_stray_quote_writes_is_refused_naming_its_author() {
    let app = "import { stray } from \"./stray\"\n\
               import { made } from stray()\n\
               fn main() -> Int64 { return made() }\n";
    let out = authors_app("stray", app);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("stray.vyrn:2:0: `helper` is exported"),
        "{err}"
    );
    assert!(
        err.contains("note: in generated code generated by stray()"),
        "{err}"
    );
}

/// An import a quote writes is judged as the module that holds the quote, even
/// when another generator splices the quote's `Code` into its own.
#[test]
fn an_import_a_quote_writes_is_judged_as_the_quotes_module() {
    for (tag, gen, from) in [("direct", "make", "./gen"), ("wrapped", "wrap", "./wrap")] {
        let app = format!(
            "import {{ {gen} }} from \"{from}\"\n\
             import {{ made }} from {gen}()\n\
             fn main() -> Int64 {{ return made() }}\n"
        );
        let out = authors_app(tag, &app);
        let err = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(7), "{tag}:\n{err}");
    }
}

/// An import a generator copies from its input is written by the input, so it
/// is judged as the input and refused at its line there.
#[test]
fn an_import_a_generator_copies_is_judged_as_its_input() {
    let app = "import { copy } from \"./gen\"\n\
               import { made } from copy(\"./script.txt\")\n\
               fn main() -> Int64 { return made() }\n";
    let out = authors_app("copied", app);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("script.txt:1:0: `helper` is exported from `./lib` only to `./gen`"),
        "{err}"
    );
}

#[test]
fn i18n_translation_with_quotes_and_backslashes_bakes_losslessly() {
    // The i18n generator bakes each translation through the `strLit` code quote,
    // so a quote or a backslash in the value must round-trip byte for byte.
    let dir = scratch("i18n_esc");
    std::fs::create_dir_all(dir.join("locales")).unwrap();
    write(
        &dir.join("locales/en.json"),
        r#"{ "quote.msg": "say \"hi\" \\ ok" }"#,
    );
    let app = "import { i18n } from \"std/i18n\"\n\
               import { tQuoteMsg } from i18n(\"./locales\")\n\
               fn main() -> Int64 { print(tQuoteMsg()) return 0 }\n";
    write(&dir.join("app.vyrn"), app);
    let out = vyrn()
        .arg("run")
        .arg(dir.join("app.vyrn"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "run failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains(r#"say "hi" \ ok"#),
        "lossless bake:\n{stdout}"
    );
}

/// A splice's tag goes on the stack before its value, so the value a field read
/// binds cannot ride the stack into it.
#[test]
fn a_spliced_field_read_is_not_carried_under_the_tag() {
    let gen = "type F = { box: String, n: Int64 }\n\
               export gen fn mk(a: String) -> Code {\n\
               let found = F { box: a + \"x\", n: 1 }\n\
               let r = render(vyrn\"\\{found.box}\")\n\
               let n = r.byteLength\n\
               return vyrn\"export fn got() -> Int64 { return \\{n} }\\n\"\n\
               }\n";
    let app = "import { mk } from \"./gen\"\n\
               import { got } from mk(\"ab\")\n\
               fn main() -> Int64 { print(got().toString()) return 0 }\n";
    let app_path = gen_app("splicefield", gen, app);
    let out = vyrn().arg("run").arg(&app_path).output().unwrap();
    assert!(
        out.status.success(),
        "run failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "5");
}

/// Runs `app` beside `gen` and returns its exit code and standard error.
fn run_gen(tag: &str, gen: &str, app: &str) -> (Option<i32>, String) {
    let out = vyrn()
        .arg("run")
        .arg(gen_app(tag, gen, app))
        .output()
        .unwrap();
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// A `Name` writes an identifier, glued to a word or alone, and `join` lists
/// `Code` values.
#[test]
fn a_name_writes_an_identifier_and_join_lists_code() {
    let gen = r#"import { join } from "std/code"
export gen fn mk() -> Code {
    let mut terms: Array<Code> = []
    for n in ["one", "two"] {
        terms.push(vyrn"get\{name(n)}()")
    }
    let fns = vyrn"fn getone() -> Int64 { return 3 }
fn gettwo() -> Int64 { return 4 }
"
    return fns + vyrn"export fn \{name("total")}() -> Int64 { return \{join(terms, vyrn" + ")} }"
}
"#;
    let app = "import { mk } from \"./gen\"\nimport { total } from mk()\nfn main() -> Int64 { return total() }\n";
    let (code, err) = run_gen("name_join", gen, app);
    assert_eq!(code, Some(7), "{err}");
}

/// A quote may be one item of a list: a record field, a `match` arm or a
/// parameter, each joined into its list.
#[test]
fn a_quote_is_one_item_of_a_list() {
    let gen = r#"import { join } from "std/code"
export gen fn mk() -> Code {
    let mut fields: Array<Code> = []
    let mut params: Array<Code> = []
    for f in ["x", "y"] {
        fields.push(vyrn"\{name(f)}: \{name(f)}")
        params.push(vyrn"\{name(f)}: Int64")
    }
    let arms: Array<Code> = [vyrn"Some(v) => v.x + v.y", vyrn"None => 0"]
    return vyrn"""
        type P = { x: Int64, y: Int64 }
        fn mkP(\{join(params, vyrn", ")}) -> P { return P { \{join(fields, vyrn", ")} } }
        export fn sum() -> Int64 {
            let o: Option<P> = Some(mkP(3, 4))
            return match o { \{join(arms, vyrn", ")} }
        }
        """
}
"#;
    let app = "import { mk } from \"./gen\"\nimport { sum } from mk()\nfn main() -> Int64 { return sum() }\n";
    let (code, err) = run_gen("items", gen, app);
    assert_eq!(code, Some(7), "{err}");
}

/// An import's list may be one joined hole and its `from` a `String` hole.
#[test]
fn a_quote_writes_an_import_from_a_string() {
    let gen = r#"import { join } from "std/code"
export gen fn mk(spec: String) -> Code {
    let names: Array<Code> = [vyrn"seven"]
    return vyrn"""
        import { \{join(names, vyrn", ")} } from \{spec}
        export fn got() -> Int64 { return seven() }
        """
}
"#;
    let app = "import { mk } from \"./gen\"\nimport { got } from mk(\"./lib\")\nfn main() -> Int64 { return got() }\n";
    let app = gen_app("import_from", gen, app);
    write(
        &app.with_file_name("lib.vyrn"),
        "export fn seven() -> Int64 { return 7 }\n",
    );
    let out = vyrn().arg("run").arg(&app).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(7), "{err}");
}

/// A one-line quote of an escaped newline keeps it: only a `"""` quote dedents.
#[test]
fn a_newline_separator_survives() {
    let gen = r#"import { join } from "std/code"
export gen fn mk() -> Code {
    let fns: Array<Code> = [vyrn"fn one() -> Int64 { return 3 }", vyrn"export fn two() -> Int64 { return one() + 4 }"]
    return join(fns, vyrn"\n")
}
"#;
    let app = "import { mk } from \"./gen\"\nimport { two } from mk()\nfn main() -> Int64 { return two() }\n";
    let out = vyrn()
        .arg("emit-gen")
        .arg(gen_app("newline", gen, app))
        .output()
        .unwrap();
    let src = String::from_utf8_lossy(&out.stdout);
    assert!(src.contains("return 3 }\nexport fn two"), "{src}");
}

/// `derive` runs a generator that returns `Code`.
#[test]
fn derive_runs_a_code_generator() {
    let app = r#"export gen fn kindOf(t: TypeArg) -> Code {
    let n = t.nodes[t.roots[0]]
    return vyrn"fn \{name(n.shape.name)}(v: \{n.code}) -> String { return \{n.shape.kind} }"
}

type Point = { x: Int64, y: Int64 }

fn main() -> Int64 {
    print(derive(kindOf, Point { x: 1, y: 2 }))
    return 0
}
"#;
    let out = vyrn()
        .arg("run")
        .arg(gen_app("derive_code", "", app))
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(String::from_utf8_lossy(&out.stdout), "record\n", "{err}");
}

/// A reflected type's `code` writes a type no identifier spells, and only a
/// type the checker resolved.
#[test]
fn a_derive_generator_splices_each_reflected_type_as_code() {
    let app = r#"import { join } from "std/code"

export gen fn kindOf(t: TypeArg) -> Code {
    let mut fns: Array<Code> = []
    for r in t.roots {
        let n = t.nodes[r]
        fns.push(vyrn"fn \{name(n.shape.name)}(v: \{n.code}) -> String { return \{n.shape.kind} }")
    }
    return join(fns, vyrn"\n")
}

type Point = { x: Int64, y: Int64 }

fn main() -> Int64 {
    let pts: Array<Point> = [Point { x: 1, y: 2 }]
    print(derive(kindOf, pts))
    let m: Map<String, Int64> = [:]
    print(derive(kindOf, m))
    let r: Result<Int64, String> = Ok(1)
    print(derive(kindOf, r))
    return 0
}
"#;
    let out = vyrn()
        .arg("run")
        .arg(gen_app("derive_types", "", app))
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "array\nmap\nenum\n",
        "{err}"
    );
}

/// `moduleCode` pairs each shape node with its `Code`, so a generator splices a
/// generic field type whole.
#[test]
fn a_module_code_shape_node_splices_a_generic_field() {
    let gen = r#"export gen fn col(module: String) -> Code {
    let mut out = vyrn"import { Id } from \{module}\n"
    for t in moduleCode(module).types {
        if t.info.name == "Row" {
            let n = t.nodes[t.info.shape[0].members[0].args[0]]
            out = out + vyrn"export fn count(v: Array<\{n.code}>) -> Int64 { return v.length }"
        }
    }
    return out
}
"#;
    let app = "import { col } from \"./gen\"\nimport { count } from col(\"./rows\")\nfn main() -> Int64 {\n    let xs: Array<Array<Option<Int64>>> = [[Some(1), None], []]\n    return count(xs)\n}\n";
    let path = gen_app("shape_node", gen, app);
    write(
        &path.with_file_name("rows.vyrn"),
        "export type Id = Int64\nexport type Row = { tags: Array<Option<Id>> }\n",
    );
    let out = vyrn().arg("emit-gen").arg(&path).output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("export fn count(v: Array<Array<Option<Id>>>) -> Int64"),
        "{text}"
    );
    let out = vyrn().arg("run").arg(&path).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "{err}");
}

/// A hole in a doc comment writes its `String` as plain text, and a newline
/// in it continues the comment, so the text cannot become code.
#[test]
fn a_comment_hole_writes_plain_text_and_stays_in_the_comment() {
    let gen = r#"export gen fn mk(doc: String) -> Code {
    return vyrn"""
        /// Says \{doc}.
        export fn four() -> Int64 { return 4 }
        """
}
"#;
    let app = "import { mk } from \"./gen\"\nimport { four } from mk(\"\\\"hi\\\"\nexport fn evil() -> Int64 { return 0 }\")\nfn main() -> Int64 { return four() }\n";
    let path = gen_app("comment_hole", gen, app);
    let out = vyrn().arg("emit-gen").arg(&path).output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains(
            "/// Says \"hi\"\n/// export fn evil() -> Int64 { return 0 }.\nexport fn four()"
        ),
        "{text}"
    );
    let out = vyrn().arg("run").arg(&path).output().unwrap();
    assert_eq!(
        out.status.code(),
        Some(4),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A derive generator's own program may call `derive`: its `toJson` runs
/// `std/json`'s generator in a load one deeper.
#[test]
fn a_derive_generator_may_call_to_json() {
    let app = r#"export gen fn tagged(t: TypeArg) -> Code {
    let mut out = vyrn""
    for r in t.roots {
        let n = t.nodes[r]
        let json = toJson(n.shape.kind)
        out = out + vyrn"fn \{name(n.shape.name)}(v: \{n.code}) -> String { return \{json} }\n"
    }
    return out
}

type Point = { x: Int64, y: Int64 }

fn main() -> Int64 {
    print(derive(tagged, Point { x: 1, y: 2 }))
    return 0
}
"#;
    let out = vyrn()
        .arg("run")
        .arg(gen_app("derive_nested", "", app))
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "\"record\"
",
        "{err}"
    );
}

/// A quote that closes a bracket it did not open is no unit, though a wrapper
/// would pair it.
#[test]
fn a_quote_that_closes_what_it_did_not_open_is_refused() {
    let gen = "export gen fn mk() -> Code {\n    return vyrn\") {} fn g(\"\n}\n";
    let app =
        "import { mk } from \"./gen\"\nimport { g } from mk()\nfn main() -> Int64 { return 0 }\n";
    let out = vyrn()
        .arg("check")
        .arg(gen_app("unbalanced", gen, app))
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("skeleton does not parse"), "{err}");
}

/// A multi-line quote drops its closing indentation, a hole alone on its line
/// takes declarations, and a spliced quote is re-indented to its hole.
#[test]
fn a_quote_dedents_and_a_hole_takes_declarations() {
    let gen = r#"export gen fn mk() -> Code {
    let body = vyrn"""
        let a = 3
        return a + 4
        """
    let item = vyrn"""
        fn helper() -> Int64 {
            \{body}
        }
        """
    return vyrn"""
        \{item}
        export fn made() -> Int64 { return helper() }
        """
}
"#;
    let app = "import { mk } from \"./gen\"\nimport { made } from mk()\nfn main() -> Int64 { return made() }\n";
    let dir = scratch("dedent");
    write(&dir.join("gen.vyrn"), gen);
    write(&dir.join("app.vyrn"), app);
    let out = vyrn()
        .arg("emit-gen")
        .arg(dir.join("app.vyrn"))
        .output()
        .unwrap();
    let src = String::from_utf8_lossy(&out.stdout);
    assert!(
        src.contains("fn helper() -> Int64 {\n    let a = 3\n    return a + 4\n}\n"),
        "{src}"
    );
    let (code, err) = run_gen("dedent_run", gen, app);
    assert_eq!(code, Some(7), "{err}");
}

/// A report is `Code` the loader reads as a diagnostic.
#[test]
fn a_report_is_read_as_a_diagnostic() {
    let gen = r#"import { reportHere } from "std/diag"
export gen fn loud() -> Code {
    return reportHere(Error, "the input is broken") + vyrn"export fn made() -> Int64 { return 7 }"
}
"#;
    let app = "import { loud } from \"./gen\"\nimport { made } from loud()\nfn main() -> Int64 { return made() }\n";
    let (code, err) = run_gen("report_loud", gen, app);
    assert_eq!(code, Some(1));
    assert!(err.contains("the input is broken"), "{err}");
}

/// `codeAt` copies input the generator read, and its errors show in the input.
#[test]
fn code_at_copies_read_input_and_shows_its_errors_there() {
    let dir = scratch("codeat");
    write(
        &dir.join("in.txt"),
        "header\nexport fn made() -> Int64 { return \"x\" }\n",
    );
    let gen = r#"export gen fn copy(path: String) -> Code {
    let text = match readFile(path) {
        Ok(t) => t,
        Err(e) => panic(e),
    }
    let line = "export fn made() -> Int64 { return \"x\" }"
    if text.byteLength == 0 {
        return vyrn""
    }
    return codeAt(path, 2, 1, line)
}
"#;
    write(&dir.join("gen.vyrn"), gen);
    write(&dir.join("app.vyrn"), "import { copy } from \"./gen\"\nimport { made } from copy(\"./in.txt\")\nfn main() -> Int64 { return made() }\n");
    let out = vyrn()
        .arg("check")
        .arg(dir.join("app.vyrn"))
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("in.txt:2:1"), "{err}");
}

/// `at` moves where an error in generated code shows, to the input it came from.
#[test]
fn at_shows_an_error_in_the_input() {
    let dir = scratch("at");
    write(&dir.join("in.txt"), "one\ntwo\nthree\n");
    let gen = r#"export gen fn mk(path: String) -> Code {
    return at(vyrn"export fn f() -> String { return 1 }", path, 3, 2)
}
"#;
    write(&dir.join("gen.vyrn"), gen);
    write(&dir.join("app.vyrn"), "import { mk } from \"./gen\"\nimport { f } from mk(\"./in.txt\")\nfn main() -> Int64 { return 0 }\n");
    let out = vyrn()
        .arg("check")
        .arg(dir.join("app.vyrn"))
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("in.txt:3:2"), "{err}");
}

/// `at` stays the builtin in a generator that imports a type with an `at`
/// projection (`std/json`'s `Json`).
#[test]
fn at_is_the_builtin_beside_a_projection_named_at() {
    let dir = scratch("at-json");
    write(&dir.join("in.txt"), "one\ntwo\nthree\n");
    let gen = r#"import { Json } from "std/json"
export gen fn mk(path: String) -> Code {
    return at(vyrn"export fn f() -> String { return 1 }", path, 3, 2)
}
"#;
    write(&dir.join("gen.vyrn"), gen);
    write(&dir.join("app.vyrn"), "import { mk } from \"./gen\"\nimport { f } from mk(\"./in.txt\")\nfn main() -> Int64 { return 0 }\n");
    let out = vyrn()
        .arg("check")
        .arg(dir.join("app.vyrn"))
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("in.txt:3:2") && !err.contains("internal error"),
        "{err}"
    );
}

/// A relative specifier a quote writes resolves beside the module that holds
/// the quote, not beside the file that called the generator.
#[test]
fn a_quote_specifier_resolves_beside_its_own_module() {
    let dir = scratch("spec-quote");
    std::fs::create_dir_all(dir.join("lib")).unwrap();
    write(
        &dir.join("lib/seven.vyrn"),
        "export fn seven() -> Int64 { return 7 }\n",
    );
    let gen = "export gen fn mk() -> Code {\n    return vyrn\"\"\"\n        import { seven } from \"./seven\"\n        export fn made() -> Int64 { return seven() }\n\n        \"\"\"\n}\n";
    write(&dir.join("lib/g.vyrn"), gen);
    write(&dir.join("app.vyrn"), "import { mk } from \"./lib/g\"\nimport { made } from mk()\nfn main() -> Int64 { return made() }\n");
    let out = vyrn()
        .arg("check")
        .arg(dir.join("app.vyrn"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A relative specifier `codeAt` copied resolves beside the input file it came
/// from.
#[test]
fn a_copied_specifier_resolves_beside_its_input() {
    let dir = scratch("spec-copy");
    std::fs::create_dir_all(dir.join("in")).unwrap();
    write(
        &dir.join("in/seven.vyrn"),
        "export fn seven() -> Int64 { return 7 }\n",
    );
    write(
        &dir.join("in/page.txt"),
        "import { seven } from \"./seven\"\n",
    );
    let gen = r#"export gen fn copy(path: String) -> Code {
    let line = match readFile(path) {
        Ok(_) => "import { seven } from \"./seven\"",
        Err(e) => panic(e),
    }
    return codeAt(path, 1, 1, line) + vyrn"""

        export fn made() -> Int64 { return seven() }

        """
}
"#;
    write(&dir.join("gen.vyrn"), gen);
    write(&dir.join("app.vyrn"), "import { copy } from \"./gen\"\nimport { made } from copy(\"./in/page.txt\")\nfn main() -> Int64 { return made() }\n");
    let out = vyrn()
        .arg("check")
        .arg(dir.join("app.vyrn"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
