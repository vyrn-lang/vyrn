//! Builtin signatures. A builtin's contract is an `ast::Function`
//! row built here, keyed by the name the call site carries (`@push`, `@pop`
//! and the other `@` names are unlexable internal spellings). A parameter's
//! capability says whether the argument is read, modified or consumed; a body
//! yielding [`ELEM`] of a parameter says the result lends; the return type is
//! what the declared reading puts on a binding. A parameter spelled `Unit` is
//! inert (a union or a type name no signature spells), and so is a lending
//! row's result; [`checkable`] and [`returns`] skip them. A bound
//! ([`HEAPLESS`], [`DECODABLE`], `Show`) states a rule about a type argument.
//! `value`, `@list` and `pullAt` allocate but have no row, because their
//! result type is one no signature spells.

use crate::ast::{Block, Capability, Expr, Function, Param, Stmt, Type, TypeDecl};
use crate::project::ELEM;
use std::sync::OnceLock;

/// The language's prelude, as Vyrn source. Embedded so a bare file with no
/// std root still gets these declarations.
const PRELUDE_SRC: &str = include_str!("prelude.vyrn");

/// Returns the types the compiler puts into every program, the ones builtin
/// rows name (`Value`, `Schema`, `ModuleInterface`, ...). Each carries line 0,
/// which is how `loader::is_injected` and the editor's symbol index recognise
/// one.
pub fn type_decls() -> &'static [TypeDecl] {
    static DECLS: OnceLock<Vec<TypeDecl>> = OnceLock::new();
    DECLS.get_or_init(|| {
        let tokens = crate::lexer::lex(PRELUDE_SRC).expect("the prelude lexes");
        let (mut program, errors) = crate::parser::parse_bare(tokens);
        assert!(
            errors.is_empty(),
            "the prelude does not parse: {}",
            errors[0].render()
        );
        for t in &mut program.type_decls {
            t.line = 0;
        }
        program.type_decls
    })
}

/// One seeded signature. `place` is empty for a row that allocates its result;
/// for a lending row it is the argument list of the [`ELEM`] the body yields
/// (a parameter name or a decimal literal), so `["self", "i"]` is `a[i]`.
fn row(
    name: &str,
    type_params: &[&str],
    params: &[(&str, Capability, Type)],
    ret: Type,
    place: &[&str],
) -> Function {
    Function {
        name: name.to_string(),
        exported: false,
        module: None,
        doc: None,
        type_params: type_params.iter().map(|s| s.to_string()).collect(),
        type_bounds: Default::default(),
        params: params
            .iter()
            .map(|(n, c, t)| Param {
                name: n.to_string(),
                capability: *c,
                ty: t.clone(),
                line: 0,
                col: 0,
            })
            .collect(),
        ret,
        body: Block {
            stmts: match place.is_empty() {
                true => Vec::new(),
                false => vec![Stmt::Return {
                    value: Some(Expr::Call {
                        dot: false,
                        type_args: Vec::new(),
                        name: ELEM.to_string(),
                        args: place
                            .iter()
                            .map(|a| match a.parse::<i64>() {
                                Ok(n) => Expr::Int(n),
                                Err(_) => Expr::Var {
                                    name: (*a).to_string(),
                                    line: 0,
                                },
                            })
                            .collect(),
                        line: 0,
                    }),
                    line: 0,
                }],
            },
        },
        line: 0,
        col: 0,
        is_extern: false,
        is_export_extern: false,
        is_gen: false,
        is_mut: false,
    }
}

/// The bound "this type owns no heap", on `@clear`, `@append` and `@copyFrom`,
/// which forget or overwrite elements without releasing them. Unlexable, so no
/// program can declare or name it; its refusal has its own wording.
pub const HEAPLESS: &str = "@Heapless";

/// The bound "JSON can decode into this type" (`crate::codec::decodable`), on
/// `fromJson`. Unlexable like [`HEAPLESS`]; its refusal names the offending
/// part of the type.
pub const DECODABLE: &str = "@Decodable";

fn bounded(mut f: Function, tp: &str, bound: &str) -> Function {
    f.type_bounds
        .insert(tp.to_string(), vec![bound.to_string()]);
    f
}

fn rows() -> Vec<Function> {
    use Capability::{Consume, Modify, Read};
    use Type::{Bool, Float, Int, Str, Unit};
    let t = || Type::Param("T".to_string());
    let arr = |e: Type| Type::Array(Box::new(e));
    let opt = |e: Type| Type::option(e);
    let stm = |e: Type| Type::Stream(Box::new(e));
    let u8s = || {
        arr(Type::IntN {
            bits: 8,
            signed: false,
        })
    };
    let u64_ = || Type::IntN {
        bits: 64,
        signed: false,
    };
    let step = || Type::Fn(vec![Int, Int, Bool], Box::new(opt(t())));
    vec![
        // The builtin containers' `place at` / `place atSet`. The body names no
        // type, so one pair serves every container and each backend types
        // [`ELEM`] itself; the `Unit` types are inert.
        row(
            "at",
            &[],
            &[("self", Read, Unit), ("i", Read, Int)],
            Unit,
            &["self", "i"],
        ),
        row(
            "atSet",
            &[],
            &[("self", Modify, Unit), ("i", Read, Int)],
            Unit,
            &["self", "i"],
        ),
        // `bytes` copies (`__vyrn_str_bytes_range` allocates), so it does not
        // lend and its result is owned. One row serves both arities: the
        // offsets change nothing about ownership.
        row("bytes", &[], &[("s", Read, Str)], u8s(), &[]),
        // A `Result`, because the bytes may not be UTF-8. Spelling it `String`
        // released the aggregate as a String buffer and crashed native code.
        row(
            "stringFromBytes",
            &[],
            &[("b", Read, u8s())],
            Type::result(Str, Str),
            &[],
        ),
        row("floatBits", &[], &[("x", Read, Float)], u64_(), &[]),
        row("floatFromBits", &[], &[("b", Read, u64_())], Float, &[]),
        row("parse", &[], &[("s", Read, Str)], opt(Int), &[]),
        row("logger", &[], &[("name", Read, Str)], Type::Logger, &[]),
        // The 1-based line and column of a byte offset in a UTF-8 buffer. The
        // parameter must be `Array<UInt8>`: over wider elements the byte offset
        // means nothing. Both route to `std/text`'s `lineAtV` and `colAtV`
        // (`loader::RT_MODULES`), which carry this signature.
        row(
            "lineAt",
            &[],
            &[("b", Read, u8s()), ("off", Read, Int)],
            Int,
            &[],
        ),
        row(
            "colAt",
            &[],
            &[("b", Read, u8s()), ("off", Read, Int)],
            Int,
            &[],
        ),
        // The number of Unicode scalar values, O(n).
        row("@charCount", &[], &[("s", Read, Str)], Int, &[]),
        // `panic` diverges; with no `Never` type the return is spelled `Unit`.
        row("panic", &[], &[("m", Read, Str)], Unit, &[]),
        row("assert", &[], &[("c", Read, Bool)], Unit, &[]),
        row(
            "assertEq",
            &["T"],
            &[("a", Read, t()), ("b", Read, t())],
            Unit,
            &[],
        ),
        row("blackBox", &["T"], &[("x", Read, t())], t(), &[]),
        // `modify`: both write the array back, so the binding must be `mut`.
        row("@pop", &["T"], &[("self", Modify, arr(t()))], opt(t()), &[]),
        row(
            "@swapRemove",
            &["T"],
            &[("self", Modify, arr(t())), ("i", Read, Int)],
            t(),
            &[],
        ),
        // The pushed value goes into the array. The receiver is `read` because
        // `push` rebuilds the array rather than mutating it (see [`rebuilds`]).
        row(
            "@push",
            &["T"],
            &[("self", Read, arr(t())), ("v", Consume, t())],
            arr(t()),
            &[],
        ),
        // Rebuilds like `push`: the result carries the possibly reallocated
        // buffer and the statement form writes it back. A named array type
        // (`type Buf = Array<Int64>`) survives through the ordinary coercion.
        row(
            "@reserve",
            &["T"],
            &[("self", Read, arr(t())), ("n", Read, Int)],
            arr(t()),
            &[],
        ),
        // The [`HEAPLESS`] rows. `clear` keeps the buffer for the next fill.
        // `append` copies its source's elements in by bytes.
        bounded(
            row("@clear", &["T"], &[("self", Read, arr(t()))], arr(t()), &[]),
            "T",
            HEAPLESS,
        ),
        bounded(
            row(
                "@append",
                &["T"],
                &[("self", Read, arr(t())), ("xs", Read, arr(t()))],
                arr(t()),
                &[],
            ),
            "T",
            HEAPLESS,
        ),
        bounded(
            row(
                "@copyFrom",
                &["T"],
                &[("self", Read, arr(t())), ("xs", Read, arr(t()))],
                arr(t()),
                &[],
            ),
            "T",
            HEAPLESS,
        ),
        // A stream's close frees what its producer was handed (the array's
        // buffer, or the step's capture block), so the argument is consumed.
        row(
            "fromArray",
            &["T"],
            &[("xs", Consume, arr(t()))],
            stm(t()),
            &[],
        ),
        // The pull producer: the stream carries a cursor (`slot`, `gen`) into a
        // slab in `std/stream`, and every `next` hands it to `step`. The runtime
        // dispatches by the step's signature, so it must depend on the element
        // type alone. The step's third argument, `closing`, is true exactly once,
        // when `close` asks it to give its slot back.
        row(
            "fromStep",
            &["T"],
            &[
                ("slot", Read, Int),
                ("gen", Read, Int),
                ("step", Consume, step()),
            ],
            stm(t()),
            &[],
        ),
        row("close", &["T"], &[("s", Consume, stm(t()))], Unit, &[]),
        row("boxStream", &["T"], &[("s", Consume, stm(t()))], Int, &[]),
        // Hands the stream to the host, which pulls and writes one encoded
        // frame at a time and `close`s it when a write fails. It is a builtin
        // because the stream escapes the call that made it.
        row("serveStream", &[], &[("s", Consume, stm(Str))], Unit, &[]),
        // The argument is an address, so nothing is consumed; the result's
        // stream type carries the disposal obligation.
        row("unboxStream", &["T"], &[("a", Read, Int)], stm(t()), &[]),
        // String `a + b` and interpolation: copies both and allocates.
        row(
            "@concat",
            &[],
            &[("a", Read, Str), ("b", Read, Str)],
            Str,
            &[],
        ),
        // `Show` holds for a type the language renders and a type that declares
        // how it renders (`Checker::type_satisfies`). Both read and keep
        // nothing, so a temporary argument is the caller's to release.
        bounded(
            row("@str", &["T"], &[("x", Read, t())], Str, &[]),
            "T",
            crate::types::SHOW,
        ),
        bounded(
            row("print", &["T"], &[("x", Read, t())], Unit, &[]),
            "T",
            crate::types::SHOW,
        ),
        // Copies the keys into a new buffer. `Array<K>`, not `Array<String>`,
        // so an Int64-keyed snapshot does not release its keys as Strings.
        row(
            "@keys",
            &["K", "V"],
            &[(
                "m",
                Read,
                Type::Map(
                    Box::new(Type::Param("K".to_string())),
                    Box::new(Type::Param("V".to_string())),
                ),
            )],
            arr(Type::Param("K".to_string())),
            &[],
        ),
        // Shrinks the map in place, as `@pop` does an array.
        row(
            "@remove",
            &["K", "V"],
            &[
                (
                    "m",
                    Modify,
                    Type::Map(
                        Box::new(Type::Param("K".to_string())),
                        Box::new(Type::Param("V".to_string())),
                    ),
                ),
                ("k", Read, Type::Param("K".to_string())),
            ],
            Bool,
            &[],
        ),
        // Insert-or-add in one probe. The key is read: a miss copies it in.
        row(
            "@tally",
            &["K"],
            &[
                (
                    "m",
                    Read,
                    Type::Map(Box::new(Type::Param("K".to_string())), Box::new(Int)),
                ),
                ("k", Read, Type::Param("K".to_string())),
                ("n", Read, Int),
            ],
            Type::Map(Box::new(Type::Param("K".to_string())), Box::new(Int)),
            &[],
        ),
        // A hit compares the bytes in place; a miss builds the String key once
        // and traps on invalid UTF-8.
        row(
            "@tallyBytes",
            &[],
            &[
                ("m", Read, Type::Map(Box::new(Str), Box::new(Int))),
                (
                    "w",
                    Read,
                    arr(Type::IntN {
                        bits: 8,
                        signed: false,
                    }),
                ),
                ("n", Read, Int),
            ],
            Type::Map(Box::new(Str), Box::new(Int)),
            &[],
        ),
        // Every allocating builtin needs a row, or an unannotated binding to its
        // result has no type and leaks. `toJson`'s parameter is a union: inert.
        row("toJson", &[], &[("x", Read, Unit)], Str, &[]),
        // Both fold to literals, whose release is a no-op (`cap == 0`); the
        // rows exist so the declared reading names the type at every site.
        row("jsonSchema", &["T"], &[], Str, &[]),
        row(
            "schemaOf",
            &["T"],
            &[],
            Type::Named("Schema".to_string()),
            &[],
        ),
        bounded(
            row(
                "fromJson",
                &["T"],
                &[("s", Read, Str)],
                Type::App("Validation".to_string(), vec![t()]),
                &[],
            ),
            "T",
            DECODABLE,
        ),
        // I/O: every result, error half included, is the caller's.
        row("args", &[], &[], arr(Str), &[]),
        row("readLine", &[], &[], opt(Str), &[]),
        row(
            "readFile",
            &[],
            &[("p", Read, Str)],
            Type::result(Str, Str),
            &[],
        ),
        row(
            "readFileBytes",
            &[],
            &[("p", Read, Str)],
            Type::result(u8s(), Str),
            &[],
        ),
        row(
            "writeFileBytes",
            &[],
            &[("p", Read, Str), ("b", Read, u8s())],
            Type::result(Bool, Str),
            &[],
        ),
        row("writeStdout", &[], &[("b", Read, u8s())], Type::Unit, &[]),
        row(
            "writeFile",
            &[],
            &[("p", Read, Str), ("s", Read, Str)],
            Type::result(Bool, Str),
            &[],
        ),
        row(
            "renameFile",
            &[],
            &[("from", Read, Str), ("to", Read, Str)],
            Type::result(Bool, Str),
            &[],
        ),
        row(
            "fsyncFile",
            &[],
            &[("p", Read, Str)],
            Type::result(Bool, Str),
            &[],
        ),
        // Generation-time calls; the rows name their types for the declared
        // reading inside a `gen fn`.
        row(
            "listDir",
            &[],
            &[("p", Read, Str)],
            Type::result(arr(Str), Str),
            &[],
        ),
        // As `listDir`, with a trailing `/` on each directory entry.
        row(
            "listDirKinds",
            &[],
            &[("p", Read, Str)],
            Type::result(arr(Str), Str),
            &[],
        ),
        row(
            "moduleInterface",
            &[],
            &[("p", Read, Str)],
            Type::Named("ModuleInterface".to_string()),
            &[],
        ),
        // The argument is a contract name, not a value: inert. The checker
        // refuses anything but a declared contract name.
        row(
            "contractOf",
            &[],
            &[("c", Read, Unit)],
            Type::Named("ContractInfo".to_string()),
            &[],
        ),
    ]
    .into_iter()
    // The log levels: `log.info(m)` is `@info`. The `@` keeps a user's
    // `fn info` from inheriting the row.
    .chain(crate::ast::LOG_LEVELS.iter().map(|lvl| {
        row(
            &format!("@{lvl}"),
            &[],
            &[("l", Read, Type::Logger), ("m", Read, Str)],
            Unit,
            &[],
        )
    }))
    .collect()
}

pub fn all() -> &'static [Function] {
    use std::sync::OnceLock;
    static ROWS: OnceLock<Vec<Function>> = OnceLock::new();
    ROWS.get_or_init(rows)
}

/// Returns the seeded row for the name a call site carries. The call-site name
/// [`crate::project::AT`] reaches the row `at`.
pub fn signature(name: &str) -> Option<&'static Function> {
    let name = if name == crate::project::AT {
        "at"
    } else {
        name
    };
    all().iter().find(|f| f.name == name)
}

/// Returns the row a call site is type-checked against, as a user declaration
/// would be, or `None` for an inert row: a lending one, or one with a `Unit`
/// parameter (no builtin takes a real `Unit`, so the spelling is the marker).
pub fn checkable(name: &str) -> Option<&'static Function> {
    let f = signature(name)?;
    (!lends(name) && !f.params.iter().any(|p| p.ty == Type::Unit)).then_some(f)
}

/// Returns each row's name and result type for [`crate::declared`], skipping a
/// lending row and a bare type-parameter result, which that reading cannot
/// solve.
pub fn returns() -> impl Iterator<Item = (&'static str, &'static Type)> {
    all()
        .iter()
        .filter(|f| !lends(&f.name) && !matches!(f.ret, Type::Param(_)))
        .map(|f| (f.name.as_str(), &f.ret))
}

/// The capability parameter `i` of `name` declares.
pub fn capability(name: &str, i: usize) -> Option<Capability> {
    // `toJson` has no row. It reads its argument and keeps nothing, so a
    // call-built argument is the caller's to release.
    if name == "toJson" && i == 0 {
        return Some(Capability::Read);
    }
    signature(name)
        .and_then(|f| f.params.get(i))
        .map(|p| p.capability)
}

/// Whether `name` rebuilds its receiver: the first parameter has the result's
/// type and that type is a container (`push`, `reserve`, `tally`), so the call
/// takes the receiver. `core::call` asks it and marks the write-back exception
/// the kernel reads.
pub fn rebuilds(name: &str) -> bool {
    let Some(f) = signature(name) else {
        return false;
    };
    f.params.first().is_some_and(|p| p.ty == f.ret)
        && matches!(f.ret, Type::Array(_) | Type::SmallArray(..) | Type::Map(..))
}

/// Whether `value(arg)` boxes a copy of `arg` rather than `arg` itself: a
/// String read out of a place some other name owns (#512). A temporary is
/// the box's to take. `string` is whether the box is `StrVal` of `arg`
/// itself; a type that renders by `show` boxes the fresh String its render
/// returns.
pub fn boxes_a_copy(arg: &Expr, string: bool) -> bool {
    string && crate::project::is_place_read(arg)
}

/// Whether the result of `name` points into one of its arguments: the row's
/// body yields [`ELEM`] of a parameter. Only `at` and `atSet` do.
pub fn lends(name: &str) -> bool {
    let Some(f) = signature(name) else {
        return false;
    };
    matches!(
        f.body.stmts.last(),
        Some(Stmt::Return { value: Some(Expr::Call { name, args, .. }), .. })
            if name == ELEM
                && matches!(args.first(), Some(Expr::Var { name: v, .. })
                    if f.params.iter().any(|p| p.name == *v))
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The order matters: the linker keeps the root module's copies, so a
    /// reordered prelude moves every declaration index. Anything but a type
    /// here would enter every program.
    #[test]
    fn the_prelude_declares_fifteen_types_and_nothing_else() {
        let names: Vec<&str> = type_decls().iter().map(|t| t.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "Value",
                "Template",
                "Issue",
                "Validation",
                "LoadResult",
                "Schema",
                "Origin",
                "ParamInfo",
                "FnInfo",
                "TypeInfo",
                "ModuleInterface",
                "MemberInfo",
                "ContractInfo",
                "Request",
                "Response",
            ]
        );
        assert!(
            type_decls().iter().all(|t| t.line == 0 && !t.exported),
            "every prelude declaration is line 0 and unexported"
        );
        let tokens = crate::lexer::lex(PRELUDE_SRC).expect("the prelude lexes");
        let (p, _) = crate::parser::parse_bare(tokens);
        assert!(
            p.functions.is_empty()
                && p.imports.is_empty()
                && p.impls.is_empty()
                && p.protocols.is_empty()
                && p.contracts.is_empty(),
            "the prelude declares something that is not a type"
        );
    }

    /// A row is matched by call name, so a user function with that name would
    /// inherit its contract.
    #[test]
    fn every_seeded_name_is_reserved_or_unspellable() {
        for f in all() {
            let n = f.name.as_str();
            assert!(
                n.starts_with('@') || crate::checker::RESERVED.contains(&n),
                "`{n}` has a seeded contract but is neither reserved nor \
                 unspellable, so a user function of that name would inherit it"
            );
        }
    }

    #[test]
    fn exactly_two_rows_are_views() {
        let views: Vec<&str> = all()
            .iter()
            .map(|f| f.name.as_str())
            .filter(|n| lends(n))
            .collect();
        assert_eq!(views, vec!["at", "atSet"]);
        assert!(
            lends(crate::project::AT),
            "the call site's name reaches `at`"
        );
    }

    #[test]
    fn the_folded_return_types_are_on_the_rows() {
        let rets: Vec<(&str, String)> = returns().map(|(n, t)| (n, t.to_string())).collect();
        let of = |n: &str| {
            rets.iter()
                .find(|(k, _)| *k == n)
                .map(|(_, t)| t.clone())
                .unwrap_or_else(|| panic!("`{n}` answers no return type"))
        };
        assert_eq!(of("@concat"), "String");
        assert_eq!(of("@str"), "String");
        assert_eq!(of("@keys"), "Array<K>");
        assert_eq!(of("@push"), "Array<T>");
        assert_eq!(of("bytes"), "Array<UInt8>");
        // A lending row and a bare type-parameter result do not answer.
        for held in ["at", "atSet", "blackBox", "@swapRemove", "@join"] {
            assert!(
                !rets.iter().any(|(k, _)| *k == held),
                "`{held}` declares an inert return type and may not answer for a call"
            );
        }
    }

    /// Every reserved name that allocates a result a signature can spell
    /// answers its type.
    #[test]
    fn every_allocating_builtin_answers_its_return_type() {
        let rets: Vec<(&str, String)> = returns().map(|(n, t)| (n, t.to_string())).collect();
        let of = |n: &str| {
            rets.iter()
                .find(|(k, _)| *k == n)
                .map(|(_, t)| t.clone())
                .unwrap_or_else(|| panic!("`{n}` answers no return type"))
        };
        for (name, ty) in [
            ("toJson", "String"),
            ("jsonSchema", "String"),
            ("schemaOf", "Schema"),
            ("args", "Array<String>"),
            ("readLine", "Option<String>"),
            ("readFile", "Result<String, String>"),
            ("readFileBytes", "Result<Array<UInt8>, String>"),
            ("writeFile", "Result<Bool, String>"),
            ("writeFileBytes", "Result<Bool, String>"),
            ("writeStdout", "Unit"),
            ("renameFile", "Result<Bool, String>"),
            ("fsyncFile", "Result<Bool, String>"),
            ("stringFromBytes", "Result<String, String>"),
            ("listDir", "Result<Array<String>, String>"),
            ("listDirKinds", "Result<Array<String>, String>"),
            ("moduleInterface", "ModuleInterface"),
            ("contractOf", "ContractInfo"),
        ] {
            assert_eq!(of(name), ty, "`{name}` answers the wrong type");
        }
        assert_eq!(of("fromJson"), "Validation<T>");
        // Held back by the module doc; giving one a row is a decision.
        for held in ["value", "@list", "pullAt"] {
            assert!(
                !rets.iter().any(|(k, _)| *k == held),
                "`{held}` is held back by the audit and may not answer for a call"
            );
        }
    }

    #[test]
    fn the_census_facts_are_on_the_signatures() {
        for (name, i) in [
            ("@push", 1),
            ("fromArray", 0),
            ("fromStep", 2),
            ("close", 0),
            ("boxStream", 0),
            ("serveStream", 0),
        ] {
            assert_eq!(
                capability(name, i),
                Some(Capability::Consume),
                "`{name}` argument {i} is taken for good"
            );
        }
        for name in ["@pop", "@swapRemove", "@remove"] {
            assert_eq!(capability(name, 0), Some(Capability::Modify));
        }
    }
}
