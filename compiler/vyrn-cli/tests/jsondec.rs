//! `fromJson` pinned by a digest over every input where a decoder can differ.
//!
//! `std/jsonread` and the generated per-type walk are the only decoder, so a
//! differential test would compare it with itself. One generated program prints a
//! line per input, and the test pins the SHA-256 of the transcript. Spot pins say
//! where a change landed; the digest alone catches the rest (a `UInt16` bound
//! widened to `65536` has no spot pin).
//!
//! After a deliberate change: run the test, take `actual` from the failure, and
//! diff the transcript in `%TEMP%/vyrn-m3-jsondec/` against the previous one.

use std::path::{Path, PathBuf};
use std::process::Command;

use vyrn_frontend::hash::sha256_hex;

/// The transcript's SHA-256.
const CORPUS_DIGEST: &str = "7b5c763d82a681935b231d2f6a9f807f110116f84d375422c1e035304b1b377d";

fn repo_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn vyrn() -> Command {
    Command::new(env!("CARGO_BIN_EXE_vyrn"))
}

/// A Vyrn string literal holding `s`.
fn lit(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// One decode target and the documents to feed it.
struct Target {
    tag: &'static str,
    decls: &'static str,
    ty: &'static str,
    /// How a successful decode is rendered: the value has a static type only
    /// inside a typed function.
    show: &'static str,
    inputs: &'static [&'static str],
}

/// The documents that exercise a scalar's whole decode surface: the right kind,
/// every wrong kind, the bounds, and the syntaxes an integer target must refuse.
const NUM_INPUTS: &[&str] = &[
    "{\"v\":0}",
    "{\"v\":1}",
    "{\"v\":-1}",
    "{\"v\":1.0}",
    "{\"v\":1.5}",
    "{\"v\":-0.5}",
    "{\"v\":1e2}",
    "{\"v\":1E2}",
    "{\"v\":1e-2}",
    "{\"v\":0.1}",
    "{\"v\":127}",
    "{\"v\":128}",
    "{\"v\":-128}",
    "{\"v\":-129}",
    "{\"v\":255}",
    "{\"v\":256}",
    "{\"v\":32767}",
    "{\"v\":32768}",
    "{\"v\":-32768}",
    "{\"v\":65535}",
    "{\"v\":65536}",
    "{\"v\":2147483647}",
    "{\"v\":2147483648}",
    "{\"v\":-2147483648}",
    "{\"v\":4294967295}",
    "{\"v\":4294967296}",
    "{\"v\":9007199254740992}",
    "{\"v\":9007199254740993}",
    "{\"v\":9223372036854775807}",
    "{\"v\":9223372036854775808}",
    "{\"v\":-9223372036854775808}",
    "{\"v\":18446744073709551615}",
    "{\"v\":18446744073709551616}",
    "{\"v\":\"1\"}",
    "{\"v\":true}",
    "{\"v\":null}",
    "{\"v\":[]}",
    "{\"v\":{}}",
    "{}",
    "{\"other\":1}",
    "{\"v\":1,\"v2\":2}",
    "[1]",
    "1",
    "null",
    "\"\"",
    "",
    "   ",
    "{\"v\":01}",
    "{\"v\":+1}",
    "{\"v\":.5}",
    "{\"v\":1.}",
    "{\"v\":1e}",
    "{\"v\":1e+}",
    "{\"v\":0x10}",
    "{\"v\":Infinity}",
    "{\"v\":NaN}",
    "{\"v\":1 }",
    " {\"v\":1} ",
    "{\"v\":1} x",
    "{\"v\":1,}",
    "{,\"v\":1}",
    "{\"v\"1}",
    "{\"v\":}",
    "{\"v\"}",
    "{\"v\":1",
];

const TARGETS: &[Target] = &[
    Target {
        tag: "i64",
        decls: "type TI = { v: Int64 }",
        ty: "TI",
        show: "toJson(x)",
        inputs: NUM_INPUTS,
    },
    Target {
        tag: "i8",
        decls: "type T8 = { v: Int8 }",
        ty: "T8",
        show: "toJson(x)",
        inputs: NUM_INPUTS,
    },
    Target {
        tag: "u8",
        decls: "type TU8 = { v: UInt8 }",
        ty: "TU8",
        show: "toJson(x)",
        inputs: NUM_INPUTS,
    },
    Target {
        tag: "i16",
        decls: "type T16 = { v: Int16 }",
        ty: "T16",
        show: "toJson(x)",
        inputs: NUM_INPUTS,
    },
    Target {
        tag: "u16",
        decls: "type TU16 = { v: UInt16 }",
        ty: "TU16",
        show: "toJson(x)",
        inputs: NUM_INPUTS,
    },
    Target {
        tag: "i32",
        decls: "type T32 = { v: Int32 }",
        ty: "T32",
        show: "toJson(x)",
        inputs: NUM_INPUTS,
    },
    Target {
        tag: "u32",
        decls: "type TU32 = { v: UInt32 }",
        ty: "TU32",
        show: "toJson(x)",
        inputs: NUM_INPUTS,
    },
    Target {
        tag: "u64",
        decls: "type TU64 = { v: UInt64 }",
        ty: "TU64",
        show: "toJson(x)",
        inputs: NUM_INPUTS,
    },
    Target {
        tag: "f64",
        decls: "type TF = { v: Float64 }",
        ty: "TF",
        show: "toJson(x)",
        inputs: NUM_INPUTS,
    },
    Target {
        tag: "f32",
        decls: "type TF32 = { v: Float32 }",
        ty: "TF32",
        show: "toJson(x)",
        inputs: NUM_INPUTS,
    },
    Target {
        tag: "age",
        decls: "type Age = Int64 where value > 0 && value < 200 \n type TA = { v: Age }",
        ty: "TA",
        show: "toJson(x)",
        inputs: NUM_INPUTS,
    },
    Target {
        tag: "bool",
        decls: "type TB = { v: Bool }",
        ty: "TB",
        show: "toJson(x)",
        inputs: &[
            "{\"v\":true}",
            "{\"v\":false}",
            "{\"v\":1}",
            "{\"v\":0}",
            "{\"v\":\"true\"}",
            "{\"v\":null}",
            "{\"v\":[]}",
            "{}",
            "true",
        ],
    },
    Target {
        tag: "str",
        decls: "type TS = { v: String }",
        ty: "TS",
        show: "toJson(x)",
        inputs: &[
            "{\"v\":\"\"}",
            "{\"v\":\"a\"}",
            "{\"v\":\"a\\nb\"}",
            "{\"v\":\"a\\tb\"}",
            "{\"v\":\"a\\rb\"}",
            "{\"v\":\"a\\bb\"}",
            "{\"v\":\"a\\fb\"}",
            "{\"v\":\"a\\/b\"}",
            "{\"v\":\"a\\\\b\"}",
            "{\"v\":\"a\\\"b\"}",
            "{\"v\":\"\\u0041\"}",
            "{\"v\":\"\\u00e9\"}",
            "{\"v\":\"\\u20ac\"}",
            // A surrogate pair decodes.
            "{\"v\":\"\\uD83D\\uDE00\"}",
            "{\"v\":\"\\uD83D\"}",
            "{\"v\":\"\\uDE00\"}",
            "{\"v\":\"\\uD83Dx\"}",
            "{\"v\":\"\\u004\"}",
            "{\"v\":\"\\uZZZZ\"}",
            "{\"v\":\"\\q\"}",
            "{\"v\":\"unterminated}",
            "{\"v\":\"é\"}",
            "{\"v\":\"€\"}",
            "{\"v\":\"😀\"}",
            "{\"v\":1}",
            "{\"v\":null}",
            // A duplicate key is refused.
            "{\"v\":\"a\",\"v\":\"b\"}",
            "{\"v\":\"a\",\"w\":1,\"v\":\"c\"}",
        ],
    },
    Target {
        tag: "nest",
        decls: "type Kid = { name: String, age: Age } \n \
                type Fam = { sur: String, kids: Array<Kid>, note: Option<String> }",
        ty: "Fam",
        show: "toJson(x)",
        inputs: &[
            "{\"sur\":\"a\",\"kids\":[]}",
            "{\"sur\":\"a\",\"kids\":[],\"note\":null}",
            "{\"sur\":\"a\",\"kids\":[],\"note\":\"n\"}",
            "{\"sur\":\"a\",\"kids\":[],\"note\":7}",
            "{\"sur\":\"a\",\"kids\":[{\"name\":\"k\",\"age\":7}]}",
            "{\"sur\":\"a\",\"kids\":[{\"name\":\"k\",\"age\":0}]}",
            "{\"sur\":\"a\",\"kids\":[{\"age\":300}]}",
            // Three failures across two elements, reported element then
            // declaration order, not discovery order.
            "{\"sur\":\"a\",\"kids\":[{\"name\":\"k\",\"age\":0},{\"age\":300}]}",
            "{\"sur\":\"a\",\"kids\":[{},{},{}]}",
            "{\"sur\":7,\"kids\":{}}",
            "{}",
            "{\"kids\":[]}",
            "{\"sur\":\"a\"}",
            "{\"sur\":\"a\",\"kids\":[1,2]}",
            "{\"sur\":\"a\",\"kids\":null}",
            "{\"sur\":\"a\",\"kids\":[],\"extra\":[1,2]}",
            "[]",
            "{\"sur\":\"a\",\"kids\":[]} trailing",
            "{ bad",
            "",
        ],
    },
    Target {
        tag: "enum",
        decls: "type Color = | Red | Green",
        ty: "Color",
        show: "toJson(x)",
        inputs: &[
            "\"Red\"",
            "\"Green\"",
            "\"Blue\"",
            "\"\"",
            "{\"Red\":1}",
            "{}",
            "1",
            "null",
            "[\"Red\"]",
        ],
    },
    Target {
        tag: "shape",
        decls: "type Shape = | Dot | Circle(Int64) | Rect(Int64, Int64)",
        ty: "Shape",
        show: "toJson(x)",
        inputs: &[
            "\"Dot\"",
            "\"Circle\"",
            "{\"Circle\":3}",
            "{\"Circle\":\"3\"}",
            "{\"Circle\":null}",
            "{\"Rect\":[2,5]}",
            "{\"Rect\":[2]}",
            "{\"Rect\":[]}",
            "{\"Rect\":[2,5,9]}",
            "{\"Rect\":2}",
            "{\"Rect\":[\"a\",\"b\"]}",
            "{\"Dot\":null}",
            "{\"Circle\":3,\"Rect\":[1,2]}",
            "{\"Nope\":1}",
            "{}",
        ],
    },
    Target {
        tag: "res",
        decls: "type TR = { r: Result<Int64, String> }",
        ty: "TR",
        show: "toJson(x)",
        inputs: &[
            "{\"r\":{\"Ok\":1}}",
            "{\"r\":{\"Err\":\"e\"}}",
            "{\"r\":{\"Ok\":\"1\"}}",
            "{\"r\":{\"Err\":1}}",
            "{\"r\":\"Ok\"}",
            "{\"r\":{}}",
            "{\"r\":{\"Ok\":1,\"Err\":\"e\"}}",
            "{\"r\":null}",
            "{}",
        ],
    },
    Target {
        tag: "map",
        decls: "type IntMap = Map<String, Int64>",
        ty: "IntMap",
        show: "toJson(x)",
        inputs: &[
            "{}",
            "{\"a\":1}",
            "{\"a\":1,\"b\":2}",
            "{\"b\":2,\"a\":1}",
            "{\"a\":1,\"a\":2}",
            "{\"a\":\"x\"}",
            "{\"a\":1,\"b\":\"x\"}",
            "[]",
            "null",
        ],
    },
    Target {
        tag: "opt",
        decls: "type TO = { xs: Array<Option<Int64>> }",
        ty: "TO",
        show: "toJson(x)",
        inputs: &[
            "{\"xs\":[]}",
            "{\"xs\":[1,null,3]}",
            "{\"xs\":[null]}",
            "{\"xs\":[\"a\"]}",
            "{\"xs\":null}",
            "{}",
        ],
    },
    Target {
        tag: "deep",
        decls: "type Node = { n: Int64, kids: Array<Node> }",
        ty: "Node",
        show: "toJson(x)",
        inputs: &[
            "{\"n\":1,\"kids\":[]}",
            "{\"n\":1,\"kids\":[{\"n\":2,\"kids\":[]}]}",
            "{\"n\":1,\"kids\":[{\"n\":2,\"kids\":[{\"n\":3,\"kids\":[]}]}]}",
            "{\"n\":1,\"kids\":[{\"kids\":[]}]}",
            "{\"n\":1,\"kids\":[{\"n\":2,\"kids\":[{\"n\":\"x\",\"kids\":[]}]}]}",
        ],
    },
];

/// One Vyrn program covering the whole corpus.
///
/// `toJson(x)` needs the argument's static type, so a decode is rendered through
/// a typed function, not from the `Valid(x)` binding. Issues print as
/// `key@path: message` in accumulation order, which is part of the answer.
fn program() -> String {
    let mut src = String::from(
        "fn issue(i: Issue) -> String { return i.key + \"@\" + i.path + \": \" + i.message }\n\
         fn issues(iss: Array<Issue>) -> String {\n\
         \x20   let mut out = \"\"\n\
         \x20   let mut first = true\n\
         \x20   for i in iss {\n\
         \x20       if first { out = issue(i) first = false } else { out = out + \" | \" + issue(i) }\n\
         \x20   }\n\
         \x20   return out\n\
         }\n",
    );
    for t in TARGETS {
        src.push_str(t.decls);
        src.push('\n');
        src.push_str(&format!(
            "fn show_{0}(x: {1}) -> String {{ return {2} }}\n\
             fn dec_{0}(s: String) -> String {{\n\
             \x20   return match fromJson<{1}>(s) {{\n\
             \x20       Valid(x) => \"ok \" + show_{0}(x),\n\
             \x20       Invalid(iss) => issues(iss),\n\
             \x20   }}\n\
             }}\n",
            t.tag, t.ty, t.show
        ));
    }
    src.push_str("fn main() -> Int64 {\n");
    for t in TARGETS {
        for (i, input) in t.inputs.iter().enumerate() {
            src.push_str(&format!(
                "    print(\"{0}{i} \" + dec_{0}({1}))\n",
                t.tag,
                lit(input)
            ));
        }
    }
    src.push_str("    return 0\n}\n");
    src
}

fn rows() -> usize {
    TARGETS.iter().map(|t| t.inputs.len()).sum()
}

/// Runs the corpus and returns its transcript, leaving the program and the
/// transcript on disk for a failure to point at.
fn transcript() -> (String, PathBuf) {
    let dir = std::env::temp_dir().join("vyrn-m3-jsondec");
    std::fs::create_dir_all(&dir).unwrap();
    let prog = dir.join("corpus.vyrn");
    std::fs::write(&prog, program()).unwrap();
    let out = vyrn()
        .arg("run")
        .arg(&prog)
        .current_dir(repo_dir())
        .output()
        .expect("run the corpus");
    let stdout = String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "the corpus did not run:\n{stderr}\n{stdout}"
    );
    std::fs::write(dir.join("transcript.txt"), &stdout).unwrap();
    (stdout, dir)
}

#[test]
fn the_decode_corpus_answers_exactly_this() {
    let (stdout, dir) = transcript();
    let n = rows();
    assert_eq!(stdout.lines().count(), n, "one line per input");

    // Spot pins, keyed by target and input rather than index, so reordering the
    // corpus cannot move one. They say where the digest moved.
    let row = |tag: &str, input: &str| -> String {
        let t = TARGETS.iter().find(|t| t.tag == tag).expect("target");
        let i = t
            .inputs
            .iter()
            .position(|s| *s == input)
            .unwrap_or_else(|| panic!("input not in the {tag} corpus: {input:?}"));
        let prefix = format!("{tag}{i} ");
        stdout
            .lines()
            .find(|l| l.starts_with(&prefix))
            .map(|l| l[prefix.len()..].to_string())
            .unwrap_or_else(|| "<missing>".to_string())
    };

    // Exact integers, never through a Float64.
    assert_eq!(
        row("i64", "{\"v\":9007199254740993}"),
        "ok {\"v\":9007199254740993}"
    );
    assert_eq!(
        row("u64", "{\"v\":18446744073709551615}"),
        "ok {\"v\":18446744073709551615}"
    );
    // A sized integer one past its bound is a type issue, not a wrap.
    assert_eq!(row("i8", "{\"v\":127}"), "ok {\"v\":127}");
    assert_eq!(
        row("i8", "{\"v\":128}"),
        "json.type@v: expected integer, found number"
    );
    assert_eq!(
        row("u8", "{\"v\":-1}"),
        "json.type@v: expected integer, found number"
    );
    // Integer syntax is required for an integer target and accepted by a float one.
    assert_eq!(
        row("i64", "{\"v\":1.0}"),
        "json.type@v: expected integer, found number"
    );
    assert_eq!(row("f64", "{\"v\":1}"), "ok {\"v\":1.000000}");
    assert_eq!(row("f64", "{\"v\":-0.5}"), "ok {\"v\":-0.500000}");
    // A base-type failure suppresses the predicate: one issue, not two.
    assert_eq!(
        row("age", "{\"v\":0}"),
        "validate@v: validation failed for `Age`"
    );
    assert_eq!(
        row("age", "{\"v\":\"1\"}"),
        "json.type@v: expected integer, found string"
    );

    // Element then declaration order, not discovery order: `kids[1]`'s missing
    // `name` before its out-of-range `age`.
    assert_eq!(
        row(
            "nest",
            "{\"sur\":\"a\",\"kids\":[{\"name\":\"k\",\"age\":0},{\"age\":300}]}"
        ),
        "validate@kids[0].age: validation failed for `Age` \
         | json.missing@kids[1].name: missing required field `name` \
         | validate@kids[1].age: validation failed for `Age`"
    );
    // Six issues from three empty elements, in element then field order.
    assert_eq!(
        row("nest", "{\"sur\":\"a\",\"kids\":[{},{},{}]}"),
        "json.missing@kids[0].name: missing required field `name` \
         | json.missing@kids[0].age: missing required field `age` \
         | json.missing@kids[1].name: missing required field `name` \
         | json.missing@kids[1].age: missing required field `age` \
         | json.missing@kids[2].name: missing required field `name` \
         | json.missing@kids[2].age: missing required field `age`"
    );
    // A missing field names the field; an unknown one is ignored; an absent
    // `Option` is `None` and omitted on the way back out.
    assert_eq!(
        row("nest", "{}"),
        "json.missing@sur: missing required field `sur` \
         | json.missing@kids: missing required field `kids`"
    );
    assert_eq!(
        row("nest", "{\"sur\":\"a\",\"kids\":[],\"extra\":[1,2]}"),
        "ok {\"sur\":\"a\",\"kids\":[]}"
    );
    // A failing `Option` field records its issue and leaves the field `None`.
    assert_eq!(
        row("nest", "{\"sur\":\"a\",\"kids\":[],\"note\":7}"),
        "json.type@note: expected string, found number"
    );

    // Exactly one wire form per enum value.
    assert_eq!(row("shape", "\"Dot\""), "ok \"Dot\"");
    assert_eq!(
        row("shape", "\"Circle\""),
        "json.type@: expected one of `Dot`, `Circle`, `Rect`, found string"
    );
    assert_eq!(
        row("shape", "{\"Dot\":null}"),
        "json.type@: expected one of `Dot`, `Circle`, `Rect`, found object"
    );
    assert_eq!(
        row("shape", "{\"Circle\":3,\"Rect\":[1,2]}"),
        "json.type@: expected one of `Dot`, `Circle`, `Rect`, found object"
    );
    assert_eq!(row("shape", "{\"Rect\":[2,5]}"), "ok {\"Rect\":[2,5]}");
    // A short tuple payload is refused on arity before any member decodes: the
    // encoder never writes one, even where every member would accept `null`.
    assert_eq!(
        row("shape", "{\"Rect\":[2]}"),
        "json.type@Rect: expected array of length 2, found array of length 1"
    );

    // The strictness rules: surrogate pairs decode, duplicate keys refuse.
    assert_eq!(
        row("str", "{\"v\":\"\\uD83D\\uDE00\"}"),
        "ok {\"v\":\"\u{1f600}\"}"
    );
    assert_eq!(
        row("str", "{\"v\":\"a\",\"v\":\"b\"}"),
        "json.parse@: line 1, col 13: duplicate object key: v"
    );
    assert_eq!(
        row("nest", "{ bad"),
        "json.parse@: line 1, col 3: expected a string key"
    );
    // A lone surrogate is still refused, on either half.
    assert_eq!(
        row("str", "{\"v\":\"\\uD83D\"}"),
        "json.parse@: line 1, col 13: unpaired high surrogate in \\u escape"
    );
    assert_eq!(
        row("str", "{\"v\":\"\\uDE00\"}"),
        "json.parse@: line 1, col 13: unexpected low surrogate in \\u escape"
    );

    // A Map is a JSON object in document order; the reader refuses a
    // duplicate key.
    assert_eq!(row("map", "{\"b\":2,\"a\":1}"), "ok {\"b\":2,\"a\":1}");
    assert_eq!(
        row("map", "{\"a\":1,\"a\":2}"),
        "json.parse@: line 1, col 11: duplicate object key: a"
    );
    // A bare `Option<T>` as an array element: `null` is `None`, and it decodes.
    assert_eq!(row("opt", "{\"xs\":[1,null,3]}"), "ok {\"xs\":[1,null,3]}");
    // A self-referential type is a call rather than an inlined walk, so depth is
    // not a compile-time bound.
    assert_eq!(
        row(
            "deep",
            "{\"n\":1,\"kids\":[{\"n\":2,\"kids\":[{\"n\":3,\"kids\":[]}]}]}"
        ),
        "ok {\"n\":1,\"kids\":[{\"n\":2,\"kids\":[{\"n\":3,\"kids\":[]}]}]}"
    );
    assert_eq!(
        row(
            "deep",
            "{\"n\":1,\"kids\":[{\"n\":2,\"kids\":[{\"n\":\"x\",\"kids\":[]}]}]}"
        ),
        "json.type@kids[0].kids[0].n: expected integer, found string"
    );

    let digest = sha256_hex(stdout.as_bytes());
    assert_eq!(
        digest,
        CORPUS_DIGEST,
        "`fromJson`'s answers moved over {n} checks. The transcript is at {}. If the \
         change is deliberate, diff it against the previous one and repin.",
        dir.join("transcript.txt").display()
    );
}
