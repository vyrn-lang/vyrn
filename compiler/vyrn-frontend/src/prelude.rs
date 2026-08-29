//! The seeded prelude (RFC-0094 M1) — **a builtin's contract is its signature.**
//!
//! A builtin has a body no program can write, so it has never had a declaration
//! either. Its ownership facts lived instead in hand-written side tables:
//! `movecheck::RESERVED_SINKS` said which argument a builtin takes for good,
//! `movecheck::RESERVED_VIEWS` said which result points back into an argument, a
//! `match` on three names said which result carries a disposal obligation, and
//! `checker::mut_array_receiver` said which receiver is written through. Four
//! lists, one fact each, and every one complete only while somebody remembered
//! to extend it.
//!
//! The census (`rfcs/census-builtins.md`, Q1) counted **eleven** names carrying
//! such a fact and **two** — `boxStream` and `serveStream` — carrying it
//! nowhere at all. PR #118 is what the arrangement costs: `fromArray` moved its
//! argument, said so in a doc comment, no rule read the doc comment, and the
//! native binary freed one buffer twice.
//!
//! ## The mechanism, and it is not new
//!
//! RFC-0091 M2 already built `ast::Function` values in Rust — with
//! `Capability::Read` and `Capability::Modify` on parameters — for the two
//! `place` rows behind `a[i]`, because `@slot` is deliberately unlexable and
//! therefore unparseable. Those two rows live here now, beside the rest, and
//! [`crate::project::seeded`] looks them up. No grammar, no embedded source
//! file, no parse step; and because nothing is imported, a bare file still runs.
//!
//! ## What a row says, and how each pass reads it
//!
//! | the fact | how the row says it | who reads it |
//! |---|---|---|
//! | this argument is taken for good | `Capability::Consume` on the parameter | `movecheck::sinks` |
//! | the result points into an argument | a body yielding [`ELEM`] of a parameter | `movecheck::views` |
//! | the result must be disposed | the return type, through `Owned` | `movecheck::linear` |
//! | the receiver is written through | `Capability::Modify` on parameter 0 | `checker::mut_array_receiver` |
//! | what the result IS | the return type | `declared::Declared::new` |
//! | the access site's lowering | the whole row | `crate::project::inline` |
//!
//! A row is keyed by the name the **call site carries**, because that is what
//! every pass matches on: `@push`, `@pop` and `@swapRemove` are the internal
//! spellings the sugar produces, and no source can write them. The `at`/`atSet`
//! pair is the exception and the reason is stated on its rows: a projection is
//! selected by the receiver's type, so it is keyed by impl-method name.
//!
//! ## What is deliberately NOT here
//!
//! - **Effects.** `SPAWN_FORBIDDEN` and `COMPTIME_FORBIDDEN` are 29 rows and no
//!   signature in this language carries an effect. That is a language feature,
//!   not a milestone.
//! - **Arity and parameter types.** The checker's per-builtin arms already
//!   refuse on both, with hand-written wording that reads better than anything
//!   a generic signature check would print. The declared types below are the
//!   census's, and they are read only where the table above says so.
//!
//! ## Where a declared type is INERT, and how a reader tells
//!
//! Two rows spell a type they do not mean. A **lending** row (`at`, `atSet`,
//! `bytes`) cannot name its result: the type is the receiver's element or the
//! receiver's representation, and one row serves every container. `@str` cannot
//! name its parameter: it takes a number, a `Bool`, a `String` or a type with
//! `impl Show`, which is a union this language does not spell. Each inert type
//! is spelled `Unit` and says so on its own row.
//!
//! [`crate::declared`] therefore reads the return type of every row EXCEPT a
//! lending one, and except a row that returns a bare type parameter — `T` names
//! a type the program never wrote, and that reading has no types with which to
//! solve it.
//!
//! ## Which reserved names have NO row, and why (RFC-0096 M3)
//!
//! RFC-0094 folded four return types onto rows. It did not audit the rest, and
//! RFC-0096 M2 found what that cost: `let s = toJson(x)` leaked its String,
//! because a call with no row has no type the declared reading can put on the
//! binding. The audit is now the whole of [`crate::checker::RESERVED`], and
//! every name that ALLOCATES a result this language can spell has a row below.
//!
//! Five names allocate and are still held back. Each is held for a reason
//! about the TYPE, never about the fact:
//!
//! | name | it answers | why no row |
//! |---|---|---|
//! | `fromJson` | `Validation<T>` | `T` is the first ARGUMENT — a type name, not a value. No signature says "the type my caller wrote". |
//! | `value` | `Value` | It boxes the caller's buffer rather than copying it (`box_payload` of the lowered argument), so it LENDS, and a row would double free. |
//! | `@list` | `Array<E>` | `E` is the argument's element type, which one row cannot name any more than `at` can. |
//! | `pullAt` | `Option<T>` | The element type comes from the expected type; the checker refuses the call without an annotation, so the binding is already named. |
//! | `at`, `atSet`, `bytes` | the receiver's | They LEND, which is the older rule above. |
//!
//! The remaining reserved names answer a scalar, `Unit`, a `Logger`, a vector or
//! a bare type parameter, and none of those owns heap.
//!
//! ## The three the audit held for the WRONG reason (RFC-0096 M3, lane C)
//!
//! `moduleInterface`, `contractOf` and `listDir` were held back together, and
//! the reason recorded for all three was that no compiling backend lowers them.
//! That is a fact about LOWERING. Every other exclusion above is a fact about
//! the type, and these three each answer a type this language spells: `listDir`
//! answers `Result<Array<String>, String>` the way `readFile` answers
//! `Result<String, String>`, and the two reflection names answer records the
//! parser INJECTS into every program (`ModuleInterface`, `ContractInfo` — see
//! `parser`'s reflection declarations). They have rows below.

use crate::ast::{Block, Capability, Expr, Function, Param, Stmt, Type};
use crate::project::ELEM;

/// One seeded signature.
///
/// `place` is empty for a row that allocates its result. For a row that LENDS
/// it, `place` is the argument list of the [`ELEM`] the body yields — a
/// parameter name, or a decimal literal — so `["self", "i"]` is `a[i]` and
/// `["s", "0"]` is the head of a String's buffer.
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
            })
            .collect(),
        ret,
        body: Block {
            stmts: match place.is_empty() {
                true => Vec::new(),
                false => vec![Stmt::Return {
                    value: Some(Expr::Call {
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

/// The seeded signatures, in the census's order (`rfcs/census-builtins.md`, the
/// prelude-extern bucket plus the three stream primitives whose ownership fact
/// M1 writes down for the first time).
fn rows() -> Vec<Function> {
    use Capability::{Consume, Modify, Read};
    use Type::{Bool, Float, Int, Str, Unit};
    let t = || Type::Param("T".to_string());
    let arr = |e: Type| Type::Array(Box::new(e));
    let opt = |e: Type| Type::Option(Box::new(e));
    let stm = |e: Type| Type::Stream(Box::new(e));
    let u8s = || {
        arr(Type::IntN {
            bits: 8,
            signed: false,
        })
    };
    let step = || Type::Fn(vec![Int, Int, Bool], Box::new(opt(t())));
    vec![
        // ---- the seeded `impl Index for <builtin container>` (RFC-0091 M2) --
        // What a builtin container's `place at` would say if it could be
        // written:
        //
        //     impl<T> Index for Array<T> {
        //         place at(read self, i: Int64) -> T      { yield @slot(self, i) }
        //         place atSet(modify self, i: Int64) -> T { yield @slot(self, i) }
        //     }
        //
        // One pair serves every builtin container. The body names no type, so
        // `Array<T>`, `SmallArray<T, N>`, `Array<T, N>`, `String` and
        // `Map<String, V>` all project the same way and each backend types
        // [`ELEM`] for itself. The declared types are therefore inert; they are
        // spelled `Unit` and never read. [`crate::project::seeded`] is the
        // lookup the access sites take.
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
        // `bytes` COPIES, and this row saying otherwise was a leak per call
        // (RFC-0114 §25's exit-residue census, first triage, 2026-08-29).
        // The row's old body claimed a view — "a header over the String's OWN
        // buffer" — and `lends` read that claim, so no binding to a `bytes`
        // result was ever released. But every engine implements a copy: the
        // interpreter must (an `Rc<Vec<Val>>` cannot share a String's bytes),
        // and `__vyrn_str_bytes_range` is a malloc and a byte loop in both
        // compiled backends. Copy is therefore the SEMANTICS, the result is
        // owned like any fresh allocation, and the ownership machinery frees
        // it wherever it frees one. A true zero-copy view is exactly
        // RFC-0109's stored-view question, and would arrive through that
        // door — a locator, not a reclassification.
        // One row for both arities (RFC-0113): the offsets are plain
        // `Int64`s and change nothing about ownership.
        row("bytes", &[], &[("s", Read, Str)], u8s(), &[]),
        // Its inverse ALLOCATES, so it is not a view. The two sit side by side on
        // purpose: the old list held one and not the other, and nothing in either
        // name says which.
        //
        // It answers a `Result`, not a `String` — the bytes may not be UTF-8, and
        // the canonical `@.io.*` wording is the error half. RFC-0094 M1 wrote
        // `String` here, which is the fork this row exists to prevent: the
        // declared reading then released `let s = stringFromBytes(b)` as a String
        // buffer, and the native binary handed `__vyrn_str_free` the aggregate's
        // tag word. **`vyrn build` on that program SEGFAULTED**, and `vyrn why
        // --memory` said "reclaimed at block exit — freeing the String buffer"
        // about it. An annotated binding never met it, which is why nothing did.
        row(
            "stringFromBytes",
            &[],
            &[("b", Read, u8s())],
            Type::Result(Box::new(Str), Box::new(Str)),
            &[],
        ),
        row("floatBits", &[], &[("x", Read, Float)], Int, &[]),
        row("floatFromBits", &[], &[("b", Read, Int)], Float, &[]),
        row("parse", &[], &[("s", Read, Str)], opt(Int), &[]),
        // ---- control (RFC-0079, RFC-0015, RFC-0055) -------------------------
        // `panic` diverges; the language has no `Never`, so the return is spelled
        // `Unit` and no rule reads it.
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
        // ---- the array methods (RFC-0011, RFC-0056) -------------------------
        // `modify` is the fact `mut_array_receiver` used to hold by name: both
        // write the array back through the binding, so the binding must be `mut`.
        row("@pop", &["T"], &[("self", Modify, arr(t()))], opt(t()), &[]),
        row(
            "@swapRemove",
            &["T"],
            &[("self", Modify, arr(t())), ("i", Read, Int)],
            t(),
            &[],
        ),
        // `push` is seeded-protocol, not prelude (RFC-0091 M2 made the sugar
        // dispatch), and M1 leaves it alone except for the one fact it carried in
        // `RESERVED_SINKS`: argument 1 goes into the array and outlives the call.
        // The receiver is `read` because that is what the pass does today — a
        // `read` array may be pushed onto, since `push` REBUILDS rather than
        // mutating.
        row(
            "@push",
            &["T"],
            &[("self", Read, arr(t())), ("v", Consume, t())],
            arr(t()),
            &[],
        ),
        // Capacity and bulk growth (RFC-0115). Both rebuild the way `push`
        // does — the receiver is `read`, the result carries the (possibly
        // reallocated) buffer, and the statement form writes it back.
        // `append` reads its source: the elements are copied, and the checker
        // holds the element type to ones a byte copy is correct for.
        row(
            "@reserve",
            &["T"],
            &[("self", Read, arr(t())), ("n", Read, Int)],
            arr(t()),
            &[],
        ),
        row(
            "@append",
            &["T"],
            &[("self", Read, arr(t())), ("xs", Read, arr(t()))],
            arr(t()),
            &[],
        ),
        row(
            "@copyFrom",
            &["T"],
            &[("self", Read, arr(t())), ("xs", Read, arr(t()))],
            arr(t()),
            &[],
        ),
        // ---- the stream primitives (RFC-0075, RFC-0090 M3) ------------------
        // The two PR #118 rows. A stream's close frees what its producer was
        // handed — the array's buffer, or the step's capture block — so the frame
        // that handed it over may not release it a second time.
        row(
            "fromArray",
            &["T"],
            &[("xs", Consume, arr(t()))],
            stm(t()),
            &[],
        ),
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
        // The three whose `consume` the census found in prose, in three engines
        // separately, or nowhere at all. Each takes a `Stream<T>`, and a
        // `Stream<T>` is linear — so the must-use walk already refuses a second
        // hand-over, and `movecheck::sinks` reads that and stands aside. What
        // these rows change is that the fact is now WRITTEN.
        row("close", &["T"], &[("s", Consume, stm(t()))], Unit, &[]),
        row("boxStream", &["T"], &[("s", Consume, stm(t()))], Int, &[]),
        row("serveStream", &[], &[("s", Consume, stm(Str))], Unit, &[]),
        // The box's inverse. Its argument is an `Int64` address, so it consumes
        // nothing a binding could double-release; what it DOES carry is the
        // disposal obligation on its result, which the return type now says and a
        // three-name `match` in `movecheck` used to.
        row("unboxStream", &["T"], &[("a", Read, Int)], stm(t()), &[]),
        // ---- the task primitive (RFC-0095 M1) -------------------------------
        // `t.join()` awaits a task and takes it for good, which is a `consume`
        // receiver and nothing else. The fact lived as a property of the WALK
        // until now: every mention of a linear binding is a disposal there, so
        // `join` consumed by being written, and no line said it.
        //
        // Rule 1 stands aside here for the reason it stands aside for `close`
        // — see [`crate::movecheck::sinks`]. What a row cannot carry is the rest
        // of `join`'s contract: which producer pairs with it (`spawn` is a
        // keyword, not a callable name) and the disposal menu the diagnostic
        // prints. Both stay hand-written, and both are about a NAME rather than
        // about a signature.
        row(
            "@join",
            &["T"],
            &[("self", Consume, Type::Task(Box::new(t())))],
            t(),
            &[],
        ),
        // ---- the four return types (RFC-0094, `declared::builtin_returns`) ---
        // Each of these was a row in a second list that recorded ONE fact: what
        // the call gives back. A row here already carries that fact, so the
        // second list is gone and these three joined the first. `@push` needed
        // no new row — it has had one since M1, and its `Array<T>` releases the
        // way the `Array<Unit>` in the old list did.
        //
        // `@concat` is the `a + b` lowering on Strings and the interpolation
        // spine; it copies out of both arguments and allocates.
        row(
            "@concat",
            &[],
            &[("a", Read, Str), ("b", Read, Str)],
            Str,
            &[],
        ),
        // `@str` renders one value. Its parameter is a union — a number, a
        // `Bool`, a `String`, or a type with `impl Show` — and this language
        // cannot spell one, so the parameter is INERT and is spelled `Unit` for
        // the reason `at`'s is. No rule reads it: RFC-0094 M1 deliberately left
        // arity and parameter types in the checker's hand-written arms, which
        // refuse a bad receiver with better words than a signature check could.
        // What the row carries is the RETURN.
        row("@str", &[], &[("x", Read, Unit)], Str, &[]),
        // `print` takes the SAME union `@str` takes, and it had no row at all —
        // the largest hole in every pass that asks what a builtin does with its
        // argument. `rfcs/census-call-arguments.md` §3 counts it: 499 of the
        // corpus's 532 unclassifiable call-argument sites are this one name, and
        // a pass with no row cannot tell "the callee may keep it" from "nobody
        // wrote it down". The parameter is inert and spelled `Unit` for the
        // reason `@str`'s is; the checker's own arm still refuses a bad
        // argument. What the row carries is the CAPABILITY: `print` reads what
        // it is given and keeps nothing, so a temporary handed to it is the
        // caller's to release.
        row("print", &[], &[("x", Read, Unit)], Unit, &[]),
        // `m.keys()` copies the keys into a new buffer (RFC-0028), so the
        // result is the caller's and the map keeps its own. Generic over the
        // KEY too (RFC-0117): `Array<K>` is what makes an Int64-keyed map's
        // snapshot release as integers rather than as someone's pointers —
        // the seeded `Array<String>` this row used to pin was where the
        // for-loop's temp release got `str_free` for an `i64`.
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
        // `m.tally(k, n)` (RFC-0116): insert-or-add on a count map, one probe.
        // The key is READ — a hit keeps the key the map already has, a miss
        // copies this one in — and the value type is pinned to `Int64`, which
        // is what makes the add spellable in a signature. The key type is any
        // legal one (RFC-0117).
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
        // `m.tallyBytes(w, n)` (RFC-0116): the byte-keyed form. On a hit the
        // bytes are compared where they lie — no String, no validation, no
        // allocation; a miss builds the key once, and bytes that are not a
        // String trap with `stringFromBytes`'s reasons behind one wording.
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
        // ---- the rest of the allocating returns (RFC-0096 M3) ---------------
        // RFC-0094 folded FOUR return types onto rows and left the audit at
        // that. RFC-0096 M2 found the hole by building a fixture: `let s =
        // toJson(x)` leaks its String unless the binding is annotated, because a
        // name with no row has no type this reading can put on a binding. The
        // audit below is the whole reserved list, not the one name.
        //
        // A row belongs here when the call ALLOCATES a result whose type the
        // signature can spell. The exclusions are on the module comment, and
        // each is a type a signature CANNOT spell rather than a fact left out.
        //
        // `toJson(x)` renders any codable value; the argument's type is the union
        // `@str`'s is, so the parameter is spelled `Unit` and is inert for the
        // same reason.
        row("toJson", &[], &[("x", Read, Unit)], Str, &[]),
        // `jsonSchema(T)` and `schemaOf(T)` take a TYPE NAME, not a value, so the
        // parameter is inert here too — the checker's arm is what refuses
        // anything but a declared name. Both fold to a compile-time literal, and
        // a literal is data-segment storage whose release is a no-op:
        // `__vyrn_str_free` returns on `cap == 0` (RFC-0089 M1a) and a `Schema`
        // is a record of such strings. The rows are here because the READING
        // must be able to name the type either way — a reading that answers for
        // some sites and not others is the fork RFC-0094 removed.
        row("jsonSchema", &[], &[("t", Read, Unit)], Str, &[]),
        row(
            "schemaOf",
            &[],
            &[("t", Read, Unit)],
            Type::Named("Schema".to_string()),
            &[],
        ),
        // ---- the input I/O results (RFC-0014, RFC-0044) ---------------------
        // Every one allocates on the host side and hands the buffer over. The
        // error half of each `Result` is canonical Vyrn wording built at the use
        // site, so it is the caller's too.
        row("args", &[], &[], arr(Str), &[]),
        row("readLine", &[], &[], opt(Str), &[]),
        row(
            "readFile",
            &[],
            &[("p", Read, Str)],
            Type::Result(Box::new(Str), Box::new(Str)),
            &[],
        ),
        row(
            "readFileBytes",
            &[],
            &[("p", Read, Str)],
            Type::Result(Box::new(u8s()), Box::new(Str)),
            &[],
        ),
        row(
            "writeFileBytes",
            &[],
            &[("p", Read, Str), ("b", Read, u8s())],
            Type::Result(Box::new(Bool), Box::new(Str)),
            &[],
        ),
        row("writeStdout", &[], &[("b", Read, u8s())], Type::Unit, &[]),
        row(
            "writeFile",
            &[],
            &[("p", Read, Str), ("s", Read, Str)],
            Type::Result(Box::new(Bool), Box::new(Str)),
            &[],
        ),
        row(
            "renameFile",
            &[],
            &[("from", Read, Str), ("to", Read, Str)],
            Type::Result(Box::new(Bool), Box::new(Str)),
            &[],
        ),
        row(
            "fsyncFile",
            &[],
            &[("p", Read, Str)],
            Type::Result(Box::new(Bool), Box::new(Str)),
            &[],
        ),
        // ---- the generation-time results (RFC-0021, RFC-0071) ---------------
        // What these rows buy is that the declared reading — and `vyrn why
        // --memory` and the LSP over it — names a type inside a `gen fn` body,
        // which is where all 31 corpus sites are. What they cannot buy is a
        // free, and they do not need to: the generation engine is the
        // interpreter, its values are Rust values (`interp::Val`, an `Rc<String>`
        // and `Box<Val>` tree) and they drop with the frame that built them. The
        // only thing that crosses out of a generation is the generated SOURCE, a
        // `String` through `gen_cache_put`. So the leak class these three could
        // belong to is empty by construction, and the row is about facts.
        //
        // `listDir` also has a runtime: `vyrn run` lists the real filesystem
        // (`COMPTIME_FORBIDDEN` deliberately omits it, and so does the
        // interpreter's generation-only refusal). Only the two compiling
        // backends have no lowering for it.
        row(
            "listDir",
            &[],
            &[("p", Read, Str)],
            Type::Result(Box::new(arr(Str)), Box::new(Str)),
            &[],
        ),
        // `listDirKinds` (RFC-0119): the same listing, each directory entry's
        // name carrying a trailing `/`. Same type, same mediation, same row
        // reasoning as `listDir` above.
        row(
            "listDirKinds",
            &[],
            &[("p", Read, Str)],
            Type::Result(Box::new(arr(Str)), Box::new(Str)),
            &[],
        ),
        row(
            "moduleInterface",
            &[],
            &[("p", Read, Str)],
            Type::Named("ModuleInterface".to_string()),
            &[],
        ),
        // A contract NAME is a declaration and not a value, so the parameter is
        // inert here for the reason `schemaOf`'s is, and the checker's arm is
        // what refuses anything but a declared contract name.
        row(
            "contractOf",
            &[],
            &[("c", Read, Unit)],
            Type::Named("ContractInfo".to_string()),
            &[],
        ),
    ]
}

/// Every seeded row, built once.
pub fn all() -> &'static [Function] {
    use std::sync::OnceLock;
    static ROWS: OnceLock<Vec<Function>> = OnceLock::new();
    ROWS.get_or_init(rows)
}

/// The signature of the builtin a call site named, or `None` if the name is not
/// a builtin with a seeded contract.
///
/// [`crate::project::AT`] is what `a[i]` and `x.at(i)` parse to; the impl method
/// it dispatches to is still named `at`, which is what a user writes in
/// `place at` and what the row below declares. Only the call site moved.
pub fn signature(name: &str) -> Option<&'static Function> {
    let name = if name == crate::project::AT {
        "at"
    } else {
        name
    };
    all().iter().find(|f| f.name == name)
}

/// What each seeded builtin gives back, for the declared-types reading
/// ([`crate::declared`]) — the name a call site carries, and the row's `ret`.
///
/// Two kinds of row are held back, and the module comment says why each is
/// inert: a **lending** row cannot name its result, and a row returning a bare
/// type **parameter** names a type the program never wrote. A reading with no
/// types must answer `None` for both, which is what it answered before there
/// were rows at all.
pub fn returns() -> impl Iterator<Item = (&'static str, &'static Type)> {
    all()
        .iter()
        .filter(|f| !lends(&f.name) && !matches!(f.ret, Type::Param(_)))
        .map(|f| (f.name.as_str(), &f.ret))
}

/// The capability parameter `i` of `name` declares.
pub fn capability(name: &str, i: usize) -> Option<Capability> {
    signature(name)
        .and_then(|f| f.params.get(i))
        .map(|p| p.capability)
}

/// Whether the result of `name` points **into** one of its arguments.
///
/// Read off the body, which is where a projection says so: a row that yields
/// [`ELEM`] of a parameter names storage its caller does not own. `at`/`atSet`
/// say it for an element of a container and `bytes` says it for a String's
/// buffer, and they are the only rows that say it.
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

    /// A row is matched by CALL NAME. That only means "the builtin" while no
    /// user function can carry the name, and `checker::RESERVED` is what stops
    /// one.
    ///
    /// RFC-0090 M4 deleted the `get` builtin and took the name out of
    /// `RESERVED` in the same stroke. `RESERVED_VIEWS` kept it, so every user
    /// function called `get` handed back a value that owned nothing, and a
    /// `Slots<String>` read through `std/slots` leaked with no diagnostic. This
    /// is the check that was missing, moved here with the rows. `atSet` — the
    /// one row a store reaches as `Stmt::IndexSet` rather than a call — is
    /// reserved like its read half `at`: nothing stops a user from DECLARING
    /// `fn atSet(..)` and calling it free-form, and the row's lend facts would
    /// then attach to it by spelling alone.
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

    /// Two rows lend, and no third may start to by accident. `bytes` was
    /// the third until the exit-residue census caught the row lying: every
    /// engine copies, so the result is owned and released like any fresh
    /// allocation.
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

    /// The four return types RFC-0094's residue held in `declared`, and the two
    /// kinds of row that may not answer.
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
        // `@push` needed no new row. The old list said `Array<Unit>` and the row
        // says `Array<T>`; both release the array's buffer.
        assert_eq!(of("@push"), "Array<T>");
        // A lending row cannot name its result, and a bare type parameter names
        // a type the program never wrote.
        // `bytes` answers now — a copy names its result like any allocator.
        assert_eq!(of("bytes"), "Array<UInt8>");
        for held in ["at", "atSet", "blackBox", "@swapRemove", "@join"] {
            assert!(
                !rets.iter().any(|(k, _)| *k == held),
                "`{held}` declares an inert return type and may not answer for a call"
            );
        }
    }

    /// RFC-0096 M3's audit: every reserved name that ALLOCATES a result this
    /// language can spell answers a type, and the eight that are held back are
    /// held for a reason the module comment states.
    ///
    /// The row that matters most is `stringFromBytes`. RFC-0094 M1 wrote its
    /// return as `String` where the checker's arm says `Result<String, String>`,
    /// so `let s = stringFromBytes(b)` was released as a String buffer and the
    /// native binary SEGFAULTED. A second list cannot drift from a row; two
    /// spellings of one row can, and this is the assertion that says which.
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
            // The three the audit held for a reason about LOWERING rather than
            // about the type. Each answers a type this language spells, and the
            // two reflection records are the parser's own injected
            // declarations, so the reading resolves them like any other name.
            ("listDir", "Result<Array<String>, String>"),
            ("listDirKinds", "Result<Array<String>, String>"),
            ("moduleInterface", "ModuleInterface"),
            ("contractOf", "ContractInfo"),
        ] {
            assert_eq!(of(name), ty, "`{name}` answers the wrong type");
        }
        // The five held back. A row appearing here later is a decision, and it
        // has to be made at the table in the module comment.
        for held in ["fromJson", "value", "@list", "pullAt"] {
            assert!(
                !rets.iter().any(|(k, _)| *k == held),
                "`{held}` is held back by the audit and may not answer for a call"
            );
        }
    }

    /// The eleven facts the census counted, the two that lived nowhere, and
    /// `join`'s, which lived as a property of the must-use walk (RFC-0095 M1).
    #[test]
    fn the_census_facts_are_on_the_signatures() {
        for (name, i) in [
            ("@push", 1),
            ("fromArray", 0),
            ("fromStep", 2),
            ("close", 0),
            ("boxStream", 0),
            ("serveStream", 0),
            ("@join", 0),
        ] {
            assert_eq!(
                capability(name, i),
                Some(Capability::Consume),
                "`{name}` argument {i} is taken for good"
            );
        }
        for name in ["@pop", "@swapRemove"] {
            assert_eq!(capability(name, 0), Some(Capability::Modify));
        }
    }
}
