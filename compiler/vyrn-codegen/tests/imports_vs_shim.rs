//! Every import, against the C that actually defines it (RFC-0077 M1).
//!
//! The wasm import section is a set of *typed* signatures, and a wrong one is
//! not a link error the way a wrong C prototype is — it is a validation failure
//! at instantiation if we are lucky, and a misread argument if we are not. Today
//! nothing checks this: the emitter writes `declare ptr @__vyrn_vj_bool(i1)`,
//! the shim defines `VJ* __vyrn_vj_bool(int)`, and LLVM reconciles the two
//! silently by widening `i1` to `i32` on the way to wasm. A direct emitter has
//! to do that widening itself — so it had better know where the widenings are.
//!
//! This walks BOTH sides and diffs them: the `declare` lines the emitter prints,
//! mapped through [`vyrn_codegen::wasm::abi`], against the C definitions in
//! `RUNTIME_SHIM`, mapped through the C ABI for wasm32. Both sides are read from
//! the source of truth rather than transcribed, for the reason M0's clang test
//! gives — a hand-written copy is a second chance to make the same mistake in
//! both places.
//!
//! Needs no toolchain: both halves are strings this crate already contains.

use std::collections::BTreeMap;
use vyrn_codegen::toolchain::runtime_shim;
use vyrn_codegen::wasm::{Sig, ValType};

/// The libc entry points the emitter calls directly. wasi-libc is the ground
/// truth here rather than the shim, so it is written down — but written down as
/// C and mapped through the same function the shim's definitions go through, so
/// the two sides cannot disagree about what `int` means.
///
/// The hazard this pins is `size_t`: it is 4 bytes on wasm32 and 8 on every
/// native target, so a `declare` passing `i64` to a libc function that takes one
/// would be wrong on exactly one of the two targets. None of these take one —
/// `strlen`, `strncmp` and `snprintf` are the three that would, and all three
/// are wrapped in the shim as `__vyrn_*` precisely so the IR can stay 64-bit.
const LIBC: &[(&str, &str, &[&str])] = &[
    ("strcmp", "int", &["const char*", "const char*"]),
    ("strstr", "char*", &["const char*", "const char*"]),
    ("strcpy", "char*", &["char*", "const char*"]),
    ("strcat", "char*", &["char*", "const char*"]),
    ("free", "void", &["void*"]),
    ("exit", "void", &["int"]),
    ("fputs", "int", &["const char*", "void*"]),
    ("fopen", "void*", &["const char*", "const char*"]),
    ("fclose", "int", &["void*"]),
];

/// A C type as a wasm value type. `None` is `void`.
fn c_abi(ty: &str) -> Option<ValType> {
    let t = ty.trim();
    // A pointer of any kind, including a function pointer — wasm32 is ILP32.
    if t.ends_with('*') || t.contains("(*") {
        return Some(ValType::I32);
    }
    match t {
        "void" => None,
        "int" | "unsigned" | "unsigned int" => Some(ValType::I32),
        "long long" | "unsigned long long" => Some(ValType::I64),
        "double" => Some(ValType::F64),
        "float" => Some(ValType::F32),
        other => panic!("no wasm ABI for the C type {other:?} — teach this test about it"),
    }
}

/// Split on top-level commas; `void (*f)(void*, int)` is one parameter.
fn split_args(s: &str) -> Vec<String> {
    let (mut out, mut depth, mut start) = (Vec::new(), 0i32, 0usize);
    for (i, c) in s.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            ',' if depth == 0 => {
                out.push(s[start..i].trim().to_string());
                start = i + 1;
            }
            _ => {}
        }
    }
    if !s.trim().is_empty() {
        out.push(s[start..].trim().to_string());
    }
    out
}

/// One C parameter, minus its name: `const char* s` → `const char*`.
fn c_param_type(p: &str) -> String {
    if p.contains("(*") {
        return "void*".to_string(); // a function pointer, whatever it points at
    }
    let words: Vec<&str> = p.split_whitespace().collect();
    let named = words.len() > 1
        && words[words.len() - 1]
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_');
    words[..words.len() - usize::from(named)].join(" ")
}

/// Every non-`static` `__vyrn_*` function the shim DEFINES, as a wasm signature.
///
/// A definition is recognized by its body: a `__vyrn_` name, a parenthesized
/// list, and `{` immediately after it. Body lines that merely call a `__vyrn_*`
/// function are followed by `;` or `)`, never `{`.
fn shim_definitions() -> BTreeMap<String, Sig> {
    let mut out = BTreeMap::new();
    let shim = runtime_shim();
    for line in shim.lines() {
        let t = line.trim();
        if t.starts_with("static")
            || t.starts_with("extern")
            || t.starts_with('*')
            || t.starts_with("/*")
        {
            continue;
        }
        let Some(at) = t.find("__vyrn_") else {
            continue;
        };
        let Some(open) = t[at..].find('(').map(|i| i + at) else {
            continue;
        };
        let name = &t[at..open];
        if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            continue;
        }
        // The matching close paren, then `{` or this is not a definition.
        let mut depth = 0i32;
        let mut close = None;
        for (i, c) in t[open..].char_indices() {
            match c {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        close = Some(open + i);
                        break;
                    }
                }
                _ => {}
            }
        }
        let Some(close) = close else { continue };
        if t[close + 1..].trim_start().as_bytes().first() != Some(&b'{') {
            continue;
        }
        let ret = t[..at].trim();
        let args: Vec<String> = split_args(&t[open + 1..close])
            .iter()
            .map(|a| c_param_type(a))
            .filter(|a| a != "void")
            .collect();
        // Variadic: nothing to compare against, since the `declare` side is
        // skipped too. wasm has no varargs (RFC-0077 M3).
        if args.iter().any(|a| a == "...") {
            continue;
        }
        out.insert(
            name.to_string(),
            (args.iter().map(|a| c_abi(a)).collect(), c_abi(ret)),
        );
    }
    out
}

/// Every `declare` the emitter prints, as a wasm signature. `None` for the
/// variadic ones — wasm has no varargs at all, which is RFC-0077 M3's whole
/// milestone, not a signature mismatch.
///
/// This used to parse the IR here. It is [`vyrn_codegen::wasm::boundary`] now,
/// because M2i made the direct backend build its import section out of the same
/// lines: a census read twice is a census that can be read two ways.
fn emitted_declarations() -> &'static BTreeMap<String, Option<Sig>> {
    vyrn_codegen::wasm::boundary()
}

#[test]
fn every_import_matches_the_signature_its_definition_has() {
    let shim = shim_definitions();
    let declared = emitted_declarations();
    let libc: BTreeMap<String, Sig> = LIBC
        .iter()
        .map(|(n, ret, args)| {
            (
                n.to_string(),
                (args.iter().map(|a| c_abi(a)).collect(), c_abi(ret)),
            )
        })
        .collect();

    let (mut bad, mut checked, mut variadic, mut unknown) = (Vec::new(), 0, 0, Vec::new());
    for (name, sig) in declared {
        let Some(sig) = sig else {
            variadic += 1;
            continue;
        };
        let Some(want) = shim.get(name).or_else(|| libc.get(name)) else {
            unknown.push(name.clone());
            continue;
        };
        checked += 1;
        if sig != want {
            bad.push(format!("{name}: declared {sig:?}, defined {want:?}"));
        }
    }

    assert!(
        unknown.is_empty(),
        "declared but defined nowhere this test can see — a real dangling import, \
         or a definition spelled in a way `shim_definitions` misses:\n{}",
        unknown.join("\n")
    );
    assert!(
        bad.is_empty(),
        "import signatures disagree with their definitions:\n{}",
        bad.join("\n")
    );
    eprintln!(
        "imports: {checked} signatures agree with their definitions, {variadic} variadic (M3)"
    );
}

/// The one mismatch M0 found — and the milestone that removed it.
///
/// `i1` is an LLVM fiction, the shim's `int` is what is really there, and the
/// sweep above only passed because [`abi`] widens. The single crossing that
/// exercised it was `declare ptr @__vyrn_vj_bool(i1)`, and RFC-0078 M2b retired
/// the JSON DOM builders along with the shim's serializer, so the boundary now has
/// NO sub-i32 argument at all. That is worth an assertion rather than a deletion:
/// it is the shape a new one would take, and the day something reintroduces one
/// the widening had better still be there.
#[test]
fn the_i1_that_is_really_an_i32() {
    let toks = vyrn_frontend::lexer::lex("fn main() -> Int64 { return 0 }").unwrap();
    let program = vyrn_frontend::parser::parse(toks).unwrap();
    let ir = vyrn_codegen::emit(&program).unwrap();
    let narrow: Vec<&str> = ir
        .lines()
        .filter(|l| l.starts_with("declare ") && !l.contains("@llvm."))
        .filter(|l| {
            ["i1", "i8", "i16"].iter().any(|n| {
                l.contains(&format!("({n},"))
                    || l.contains(&format!("({n})"))
                    || l.contains(&format!(" {n},"))
                    || l.contains(&format!(" {n})"))
            })
        })
        .collect();
    assert!(
        narrow.is_empty(),
        "a new sub-i32 boundary type — check `abi` still widens it: {narrow:?}"
    );
}

/// No aggregate crosses the boundary — M0 measured 0, and this is the assertion
/// that keeps it 0. The day one does, the shadow-stack convention needs a
/// hidden-pointer story at the C boundary too, and it should be a red test that
/// says so rather than a surprise in M2.
#[test]
fn no_import_takes_or_returns_an_aggregate() {
    let toks = vyrn_frontend::lexer::lex("fn main() -> Int64 { return 0 }").unwrap();
    let program = vyrn_frontend::parser::parse(toks).unwrap();
    let ir = vyrn_codegen::emit(&program).unwrap();
    for line in ir.lines().filter(|l| l.starts_with("declare ")) {
        assert!(
            !line.contains('{')
                && !line.contains('[')
                && !line.contains("byval")
                && !line.contains("sret"),
            "an aggregate crosses the C boundary:\n{line}"
        );
    }
}
