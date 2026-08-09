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
        // ---- the representation views (RFC-0078 M4a) ------------------------
        // `bytes` is the one all four runtime modules stand on: the `Array<UInt8>`
        // it hands back is a header over the String's OWN buffer, so a binding to
        // it owns nothing and nothing may release it. The body says exactly that —
        // the result is the place at offset 0 of `s` — which is the sentence
        // `place at` writes for `a[i]`, and the declared return type is inert for
        // the reason it is inert there.
        row("bytes", &[], &[("s", Read, Str)], u8s(), &["s", "0"]),
        // Its inverse ALLOCATES, so it is not a view. The two sit side by side on
        // purpose: the old list held one and not the other, and nothing in either
        // name says which.
        row("stringFromBytes", &[], &[("b", Read, u8s())], Str, &[]),
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
    /// is the check that was missing, moved here with the rows.
    #[test]
    fn every_seeded_name_is_reserved_or_unspellable() {
        for f in all() {
            let n = f.name.as_str();
            assert!(
                // `atSet` is the one row keyed by IMPL METHOD rather than by
                // call: a store reaches it as `Stmt::IndexSet`, and no lowering
                // makes a call of that name. `at` is reserved and is the pair's
                // read half.
                n == "atSet" || n.starts_with('@') || crate::checker::RESERVED.contains(&n),
                "`{n}` has a seeded contract but is neither reserved nor \
                 unspellable, so a user function of that name would inherit it"
            );
        }
    }

    /// Three rows lend, and no fourth may start to by accident.
    #[test]
    fn exactly_three_rows_are_views() {
        let views: Vec<&str> = all()
            .iter()
            .map(|f| f.name.as_str())
            .filter(|n| lends(n))
            .collect();
        assert_eq!(views, vec!["at", "atSet", "bytes"]);
        assert!(
            lends(crate::project::AT),
            "the call site's name reaches `at`"
        );
    }

    /// The eleven facts the census counted, and the two that lived nowhere.
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
        for name in ["@pop", "@swapRemove"] {
            assert_eq!(capability(name, 0), Some(Capability::Modify));
        }
    }
}
