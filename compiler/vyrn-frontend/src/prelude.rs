//! The builtin table: one [`Builtin`] row per builtin, keyed by the name a
//! call site carries (`@push`, `@pop` and the other `@` names are unlexable
//! internal spellings). A builtin's contract is an `ast::Function`. A parameter's
//! capability says whether the argument is read, modified or consumed; a body
//! yielding [`ELEM`] of a parameter says the result lends; the return type is
//! what the declared reading puts on a binding. A parameter spelled `Unit` is
//! inert (a union or a type name no signature spells), and so is a lending
//! row's result; [`checkable`] and [`returns`] skip them. A bound
//! ([`HEAPLESS`], [`DECODABLE`], `Show`) states a rule about a type argument.
//! `value`, `@list` and `pullAt` allocate but have no contract, because their
//! result type is one no signature spells.

use crate::ast::{Block, Capability, Expr, Function, Id, Param, Stmt, Type, TypeDecl};
use crate::effects::Effect;
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
                id: Id::NEW,
                name: n.to_string(),
                capability: *c,
                ty: t.clone(),
                line: 0,
                col: 0,
            })
            .collect(),
        ret,
        body: Block {
            id: Id::NEW,
            stmts: match place.is_empty() {
                true => Vec::new(),
                false => vec![Stmt::Return {
                    id: Id::NEW,
                    value: Some(Expr::Call {
                        id: Id::NEW,
                        dot: false,
                        type_args: Vec::new(),
                        name: ELEM.to_string(),
                        args: place
                            .iter()
                            .map(|a| match a.parse::<i64>() {
                                Ok(n) => Expr::Int(n, Id::NEW),
                                Err(_) => Expr::Var {
                                    id: Id::NEW,
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

/// A receiver shape the editor's member completion offers builtins on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Shape {
    Array,
    ArrayN,
    SmallArray,
    Map,
    Str,
    /// A number or a `Bool`.
    Scalar,
    Stream,
    Logger,
    Record,
    Enum,
}

impl Shape {
    /// The shape of a receiver of type `ty`, or `None` for a type no builtin
    /// method is offered on.
    pub fn of(ty: &Type) -> Option<Shape> {
        Some(match ty {
            Type::Array(_) => Shape::Array,
            Type::ArrayN(..) => Shape::ArrayN,
            Type::SmallArray(..) => Shape::SmallArray,
            Type::Map(..) => Shape::Map,
            Type::Str => Shape::Str,
            Type::Int | Type::IntN { .. } | Type::Float | Type::Float32 | Type::Bool => {
                Shape::Scalar
            }
            Type::Stream(_) => Shape::Stream,
            Type::Logger => Shape::Logger,
            Type::Record(_) => Shape::Record,
            Type::Enum(_) => Shape::Enum,
            _ => return None,
        })
    }
}

/// What a call does to its receiver's length: the fact a pass that removes a
/// bounds check may carry across the call (obligation O3). Every other operand
/// is read, so it keeps its length and its elements.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Length {
    /// States nothing, so the pass forgets what it knew.
    Unknown,
    Keeps,
    GrowsByOne,
    /// By one; `@pop` of an empty array keeps it, and `@swapRemove` of one traps.
    ShrinksByOneIfNotEmpty,
    SetToZero,
    GrowsByLenOf(usize),
    SetToLenOf(usize),
}

/// What a call does to its receiver's elements, in the sense of [`Length`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Elements {
    Unknown,
    /// Every position that survives the call holds the element it held.
    KeepsEachPosition,
    /// Every element after the call is one the receiver held, at any position:
    /// `@swapRemove` moves the last element into the removed slot.
    KeepsRange,
}

/// One builtin, keyed by the name a call site carries. The checker's contract,
/// the parser's method spelling, the core's [`Spec`], the effect lattice's
/// atom, the loader's route and the editor's completion, hover and colour read
/// this row. `vyrn_codegen::direct` dispatches on the name itself.
pub struct Builtin {
    pub name: &'static str,
    /// The contract a call is checked against, as a user declaration would
    /// be. Its name is the call's, except `at`, which the call `@at` reaches.
    pub sig: Option<Function>,
    /// The `x.m(..)` spelling the parser rewrites to [`Builtin::name`] when
    /// nothing in the module answers to it.
    pub method: Option<&'static str>,
    /// The receivers completion offers the method on.
    pub on: &'static [Shape],
    /// What the core states about a call's operands and result.
    pub spec: Option<Spec>,
    pub effect: Option<Effect>,
    /// The reserved spelling of the `std` function a call becomes, with its
    /// module's prefix (`loader::RT_MODULES`), and the one a generator host
    /// calls instead.
    pub route: Option<&'static str>,
    pub gen_route: Option<&'static str>,
    /// Hover and completion text, under the method's spelling.
    pub hover: Option<String>,
    /// [`Length::Unknown`] for a row that neither takes its receiver
    /// ([`Spec::Rebuilds`]) nor shrinks it ([`Spec::Removes`]).
    pub length: Length,
    pub elements: Elements,
}

fn b(name: &'static str) -> Builtin {
    Builtin {
        name,
        sig: None,
        method: None,
        on: &[],
        spec: None,
        effect: None,
        route: None,
        gen_route: None,
        hover: None,
        length: Length::Unknown,
        elements: Elements::Unknown,
    }
}

impl Builtin {
    fn sig(self, f: Function) -> Self {
        Builtin {
            sig: Some(f),
            ..self
        }
    }
    fn method(self, m: &'static str, on: &'static [Shape]) -> Self {
        Builtin {
            method: Some(m),
            on,
            ..self
        }
    }
    /// Offers a free function in completion on `on`, with no method spelling.
    fn method_on(self, on: &'static [Shape]) -> Self {
        Builtin { on, ..self }
    }
    fn spec(self, s: Spec) -> Self {
        Builtin {
            spec: Some(s),
            ..self
        }
    }
    fn effect(self, e: Effect) -> Self {
        Builtin {
            effect: Some(e),
            ..self
        }
    }
    fn route(self, r: &'static str) -> Self {
        Builtin {
            route: Some(r),
            ..self
        }
    }
    fn gen_route(self, r: &'static str) -> Self {
        Builtin {
            gen_route: Some(r),
            ..self
        }
    }
    fn hover(self, h: &str) -> Self {
        Builtin {
            hover: Some(h.to_string()),
            ..self
        }
    }
    fn resizes(self, length: Length, elements: Elements) -> Self {
        Builtin {
            length,
            elements,
            ..self
        }
    }
}

/// The rows. Completion offers a shape's methods in this order.
fn table() -> Vec<Builtin> {
    use crate::effects::Effect::{
        Args, FsList, FsRead, FsWrite, GenOnly, ReadInput, Serve, Trap, WriteOutput,
    };
    use Capability::{Consume, Modify, Read};
    use Shape::*;
    use Type::{Bool, Float, Int, Str, Unit};
    let t = || Type::Param("T".to_string());
    let k = || Type::Param("K".to_string());
    let arr = |e: Type| Type::Array(Box::new(e));
    let opt = |e: Type| Type::option(e);
    let stm = |e: Type| Type::Stream(Box::new(e));
    let u8_ = || Type::IntN {
        bits: 8,
        signed: false,
    };
    let u64_ = || Type::IntN {
        bits: 64,
        signed: false,
    };
    let i32_ = Type::IntN {
        bits: 32,
        signed: true,
    };
    let step = || Type::Fn(vec![Int, Int, Bool], Box::new(opt(t())));
    let lists = &[Array, ArrayN, SmallArray];
    // IEEE-754 bit views and SIMD: each operand at the stated type, one
    // instruction. A bit view reads the same 64 bits at the other type, so it
    // is not a conversion.
    let one = |n, p: &Type, r: &Type| b(n).spec(Spec::Typed(vec![p.clone()], r.clone()));
    let two = |n, p: &Type, r: &Type| b(n).spec(Spec::Typed(vec![p.clone(), p.clone()], r.clone()));
    let (f4, d2) = (Type::F32x4, Type::F64x2);
    let level = |name, method| {
        b(name)
            .sig(row(
                name,
                &[],
                &[("l", Read, Type::Logger), ("m", Read, Str)],
                Unit,
                &[],
            ))
            .method(method, &[Logger])
            .spec(Spec::Logs)
            .effect(WriteOutput)
            .hover(&format!(
                "{method}(logger, message) -> Unit — log at {method} level"
            ))
    };
    let mut rows = vec![
        // The pushed value goes into the array. The receiver is `read` because
        // `push` rebuilds the array rather than mutating it (see [`rebuilds`]).
        b("@push")
            .sig(row(
                "@push",
                &["T"],
                &[("self", Read, arr(t())), ("v", Consume, t())],
                arr(t()),
                &[],
            ))
            .method("push", lists)
            .spec(Spec::Rebuilds)
            .hover("array.push(value) -> Array<T> — append to a growable array; a statement writes the result back through the receiver")
            .resizes(Length::GrowsByOne, Elements::KeepsEachPosition),
        // The builtin containers' `place at` / `place atSet`. The body names no
        // type, so one pair serves every container and each backend types
        // [`ELEM`] itself; the `Unit` types are inert. `xs[i]` is `@at` too, and
        // the impl method it dispatches to keeps the name `at`.
        b(crate::project::AT)
            .sig(row(
                "at",
                &[],
                &[("self", Read, Unit), ("i", Read, Int)],
                Unit,
                &["self", "i"],
            ))
            .method("at", lists)
            .hover("array.at(index) -> T — read an element by index; `array[index]` is the same call"),
        b("atSet").sig(row(
            "atSet",
            &[],
            &[("self", Modify, Unit), ("i", Read, Int)],
            Unit,
            &["self", "i"],
        )),
        // `modify`: both write the array back, so the binding must be `mut`.
        b("@pop")
            .sig(row("@pop", &["T"], &[("self", Modify, arr(t()))], opt(t()), &[]))
            .method("pop", &[Array, SmallArray])
            .spec(Spec::Removes)
            .hover("array.pop() -> Option<T> — remove and return the last element (None if empty)")
            .resizes(Length::ShrinksByOneIfNotEmpty, Elements::KeepsEachPosition),
        b("@swapRemove")
            .sig(row(
                "@swapRemove",
                &["T"],
                &[("self", Modify, arr(t())), ("i", Read, Int)],
                t(),
                &[],
            ))
            .method("swapRemove", &[Array, SmallArray])
            .spec(Spec::Removes)
            .hover("array.swapRemove(index) -> T — O(1) unordered remove: move the last element into the slot")
            .resizes(Length::ShrinksByOneIfNotEmpty, Elements::KeepsRange),
        // Rebuilds like `push`: the result carries the possibly reallocated
        // buffer and the statement form writes it back. A named array type
        // (`type Buf = Array<Int64>`) survives through the ordinary coercion.
        b("@reserve")
            .sig(row(
                "@reserve",
                &["T"],
                &[("self", Read, arr(t())), ("n", Read, Int)],
                arr(t()),
                &[],
            ))
            .method("reserve", &[Array])
            .spec(Spec::Rebuilds)
            .hover("array.reserve(n) -> Array<T> — make room for n more elements ahead of time, so a known-size build is one allocation")
            .resizes(Length::Keeps, Elements::KeepsEachPosition),
        // The [`HEAPLESS`] rows. `clear` keeps the buffer for the next fill.
        // `append` copies its source's elements in by bytes, and `copyFrom`
        // overwrites the receiver's, reusing its buffer.
        b("@clear")
            .sig(bounded(
                row("@clear", &["T"], &[("self", Read, arr(t()))], arr(t()), &[]),
                "T",
                HEAPLESS,
            ))
            .method("clear", &[Array])
            .spec(Spec::Rebuilds)
            .hover("array.clear() -> Array<T> — length to zero, buffer kept for the next fill; element type must not own heap")
            .resizes(Length::SetToZero, Elements::KeepsEachPosition),
        b("@append")
            .sig(bounded(
                row(
                    "@append",
                    &["T"],
                    &[("self", Read, arr(t())), ("xs", Read, arr(t()))],
                    arr(t()),
                    &[],
                ),
                "T",
                HEAPLESS,
            ))
            .method("append", &[Array])
            .spec(Spec::Rebuilds)
            .hover("array.append(other) -> Array<T> — copy every element of `other` on, in order; element type must not own heap")
            .resizes(Length::GrowsByLenOf(1), Elements::KeepsEachPosition),
        b("@copyFrom")
            .sig(bounded(
                row(
                    "@copyFrom",
                    &["T"],
                    &[("self", Read, arr(t())), ("xs", Read, arr(t()))],
                    arr(t()),
                    &[],
                ),
                "T",
                HEAPLESS,
            ))
            .method("copyFrom", &[Array])
            .spec(Spec::Rebuilds)
            .hover("array.copyFrom(src) -> Array<T> — overwrite the elements with `src`'s, reusing the buffer; element type must not own heap")
            .resizes(Length::SetToLenOf(1), Elements::Unknown),
        b("@toArray")
            .method("toArray", &[SmallArray])
            .spec(Spec::Builds(arr(t())))
            .hover("smallArray.toArray() -> Array<T> — copy a SmallArray's elements out to a growable Array"),
        b("@has")
            .method("has", &[Map])
            .spec(Spec::Finds)
            .hover("map.has(key) -> Bool — whether the map contains the key"),
        // Shrinks the map in place, as `@pop` does an array.
        b("@remove")
            .sig(row(
                "@remove",
                &["K", "V"],
                &[
                    (
                        "m",
                        Modify,
                        Type::Map(Box::new(k()), Box::new(Type::Param("V".to_string()))),
                    ),
                    ("k", Read, k()),
                ],
                Bool,
                &[],
            ))
            .method("remove", &[Map])
            .spec(Spec::Removes)
            .hover("map.remove(key) -> Bool — remove the entry (order-preserving); was it present?"),
        // Copies the keys into a new buffer. `Array<K>`, not `Array<String>`,
        // so an Int64-keyed snapshot does not release its keys as Strings. `K`
        // is the row's own parameter name, which the core's result names too.
        b("@keys")
            .sig(row(
                "@keys",
                &["K", "V"],
                &[(
                    "m",
                    Read,
                    Type::Map(Box::new(k()), Box::new(Type::Param("V".to_string()))),
                )],
                arr(k()),
                &[],
            ))
            .method("keys", &[Map])
            .spec(Spec::Builds(arr(k())))
            .hover("map.keys() -> Array<String> — a snapshot of the keys, in insertion order"),
        // `Show` holds for a type the language renders and a type that declares
        // how it renders (`Checker::type_satisfies`). Both read and keep
        // nothing, so a temporary argument is the caller's to release.
        b("@str")
            .sig(bounded(
                row("@str", &["T"], &[("x", Read, t())], Str, &[]),
                "T",
                crate::types::SHOW,
            ))
            .method("toString", &[Shape::Str, Scalar])
            .spec(Spec::Renders(Str))
            .hover("x.toString() -> String — render a number, Bool, or String"),
        // The number of Unicode scalar values, O(n). Method-only, so it has no
        // free spelling to import; it stays routed.
        b("@charCount")
            .sig(row("@charCount", &[], &[("s", Read, Str)], Int, &[]))
            .method("charCount", &[Shape::Str])
            .route("text$charCountV")
            .hover("s.charCount() -> Int64 — number of Unicode scalar values (O(n); counts non-continuation bytes)"),
        b("close")
            .sig(row("close", &["T"], &[("s", Consume, stm(t()))], Unit, &[]))
            .spec(Spec::Effect(Unit))
            .method_on(&[Stream])
            .hover("close(stream) -> Unit — discharge a stream's disposal obligation without consuming it"),
    ];
    // A seeded row is matched by name, so the unlexable `@info` keeps a user
    // `fn info(..)` from inheriting the log contract.
    rows.extend(crate::ast::LOG_LEVELS.iter().map(|&l| level(l, &l[1..])));
    rows.extend([
        // A deep copy of an owned heap value. Completion offers it where the
        // type owns heap by its shape; on a scalar it is the identity.
        b("@copy")
            .method("copy", &[Shape::Str, Array, ArrayN, SmallArray, Map, Record, Enum])
            .spec(Spec::OwnType)
            .hover("x.copy() -> T — a value of the receiver's type that shares no heap with it; deep and structural. A handle copies as the value it is, so the copy names the same thing"),
        // Insert-or-add in one probe. The key is read: a miss copies it in.
        b("@tally")
            .sig(row(
                "@tally",
                &["K"],
                &[
                    ("m", Read, Type::Map(Box::new(k()), Box::new(Int))),
                    ("k", Read, k()),
                    ("n", Read, Int),
                ],
                Type::Map(Box::new(k()), Box::new(Int)),
                &[],
            ))
            .method("tally", &[])
            .spec(Spec::Rebuilds)
            .hover("map.tally(key, n) -> Map<String, Int64> — insert-or-add on a count map, one probe"),
        // A hit compares the bytes in place; a miss builds the String key once
        // and traps on invalid UTF-8.
        b("@tallyBytes")
            .sig(row(
                "@tallyBytes",
                &[],
                &[
                    ("m", Read, Type::Map(Box::new(Str), Box::new(Int))),
                    ("w", Read, arr(u8_())),
                    ("n", Read, Int),
                ],
                Type::Map(Box::new(Str), Box::new(Int)),
                &[],
            ))
            .method("tallyBytes", &[])
            .spec(Spec::Rebuilds)
            .hover("map.tallyBytes(bytes, n) -> Map<String, Int64> — tally keyed by raw bytes; the String is built and validated only on a miss"),
        // Value methods, not `F32x4.lane(v, k)`: a value-receiver method name is
        // a global default, and `min`/`max`/`abs` are `std/math` exports, so the
        // rest of the vector surface is on the type name. `anyTrue` and
        // `allTrue` are the wasm instructions' names, which leave `any` and `all`
        // free.
        b("@lane")
            .method("lane", &[])
            .spec(Spec::Lanes)
            .hover("vector.lane(k) -> T — read lane `k` of an `F32x4`, `I32x4`, `F64x2` or mask; `k` is a compile-time constant inside the width"),
        b("@replaceLane")
            .method("replaceLane", &[])
            .spec(Spec::Lanes)
            .hover("vector.replaceLane(k, x) -> Vector — the same vector with lane `k` set to `x`; `k` is a compile-time constant inside the width"),
        b("@anyTrue")
            .method("anyTrue", &[])
            .spec(Spec::Lanes)
            .hover("mask.anyTrue() -> Bool — whether any lane of a comparison mask is set"),
        b("@allTrue")
            .method("allTrue", &[])
            .spec(Spec::Lanes)
            .hover("mask.allTrue() -> Bool — whether every lane of a comparison mask is set"),
        b("F32x4").spec(Spec::Lanes),
        b("I32x4").spec(Spec::Lanes),
        b("F64x2").spec(Spec::Lanes),
        b("@f32x4Load").spec(Spec::Lanes),
        b("@f32x4Store").spec(Spec::Lanes),
        b("@i32x4Load").spec(Spec::Lanes),
        b("@i32x4Store").spec(Spec::Lanes),
        b("@f64x2Load").spec(Spec::Lanes),
        b("@f64x2Store").spec(Spec::Lanes),
        one("@f32x4Splat", &Type::Float32, &f4),
        one("@i32x4Splat", &i32_, &Type::I32x4),
        one("@f64x2Splat", &Float, &d2),
        two("@f32x4Min", &f4, &f4),
        two("@f32x4Max", &f4, &f4),
        one("@f32x4Sqrt", &f4, &f4),
        one("@f32x4Ceil", &f4, &f4),
        one("@f32x4Floor", &f4, &f4),
        one("@f32x4Trunc", &f4, &f4),
        one("@f32x4Nearest", &f4, &f4),
        two("@f64x2Min", &d2, &d2),
        two("@f64x2Max", &d2, &d2),
        one("@f64x2Sqrt", &d2, &d2),
        one("floatBits", &Float, &u64_()).sig(row(
            "floatBits",
            &[],
            &[("x", Read, Float)],
            u64_(),
            &[],
        )),
        one("floatFromBits", &u64_(), &Float).sig(row(
            "floatFromBits",
            &[],
            &[("b", Read, u64_())],
            Float,
            &[],
        )),
        // `bytes` copies (`__vyrn_str_bytes_range` allocates), so it does not
        // lend and its result is owned. One row serves both arities: the
        // offsets change nothing about ownership.
        b("bytes")
            .sig(row("bytes", &[], &[("s", Read, Str)], arr(u8_()), &[]))
            .spec(Spec::Builds(arr(u8_()))),
        // A `Result`, because the bytes may not be UTF-8. Spelling it `String`
        // released the aggregate as a String buffer and crashed native code.
        b("stringFromBytes")
            .sig(row(
                "stringFromBytes",
                &[],
                &[("b", Read, arr(u8_()))],
                Type::result(Str, Str),
                &[],
            ))
            .spec(Spec::Builds(Type::result(Str, Str))),
        b("logger")
            .sig(row("logger", &[], &[("name", Read, Str)], Type::Logger, &[]))
            .spec(Spec::Logs),
        // The 1-based line and column of a byte offset in a UTF-8 buffer. The
        // parameter must be `Array<UInt8>`: over wider elements the byte offset
        // means nothing. The routes carry this signature.
        b("lineAt")
            .sig(row(
                "lineAt",
                &[],
                &[("b", Read, arr(u8_())), ("off", Read, Int)],
                Int,
                &[],
            ))
            .route("text$lineAtV"),
        b("colAt")
            .sig(row(
                "colAt",
                &[],
                &[("b", Read, arr(u8_())), ("off", Read, Int)],
                Int,
                &[],
            ))
            .route("text$colAtV"),
        // `panic` diverges; with no `Never` type the return is spelled `Unit`.
        b("panic")
            .sig(row("panic", &[], &[("m", Read, Str)], Unit, &[]))
            .spec(Spec::Traps)
            .effect(Trap),
        b(crate::ast::PANIC_AT).spec(Spec::Traps).effect(Trap),
        b("assert")
            .sig(row("assert", &[], &[("c", Read, Bool)], Unit, &[]))
            .spec(Spec::Asserts)
            .effect(Trap),
        b("assertEq")
            .sig(row(
                "assertEq",
                &["T"],
                &[("a", Read, t()), ("b", Read, t())],
                Unit,
                &[],
            ))
            .spec(Spec::Asserts)
            .effect(Trap),
        b("blackBox")
            .sig(row("blackBox", &["T"], &[("x", Read, t())], t(), &[]))
            .spec(Spec::Barrier),
        // A stream's close frees what its producer was handed (the array's
        // buffer, or the step's capture block), so the argument is consumed.
        // A producer builds the stream's six-word header.
        b("fromArray")
            .sig(row(
                "fromArray",
                &["T"],
                &[("xs", Consume, arr(t()))],
                stm(t()),
                &[],
            ))
            .spec(Spec::Builds(stm(t())))
            .hover("fromArray(array) -> Stream<T> — move an array's elements into a linear stream"),
        // The pull producer: the stream carries a cursor (`slot`, `gen`) into a
        // slab in `std/stream`, and every `next` hands it to `step`. The runtime
        // dispatches by the step's signature, so it must depend on the element
        // type alone. The step's third argument, `closing`, is true exactly once,
        // when `close` asks it to give its slot back.
        b("fromStep")
            .sig(row(
                "fromStep",
                &["T"],
                &[
                    ("slot", Read, Int),
                    ("gen", Read, Int),
                    ("step", Consume, step()),
                ],
                stm(t()),
                &[],
            ))
            .spec(Spec::Builds(stm(t())))
            .hover("fromStep(slot, generation, step) -> Stream<T> — a stream that pulls from `step: fn(Int64, Int64, Bool) -> Option<T>` over a cursor its caller minted; `std/stream`'s `unfold` is the one to call"),
        b("boxStream")
            .sig(row("boxStream", &["T"], &[("s", Consume, stm(t()))], Int, &[]))
            .spec(Spec::Effect(Int))
            .hover("boxStream(s) -> Int64 — move a stream into one heap box and answer its address; `std/stream` keeps a wrapper's source this way"),
        // Hands the stream to the host, which pulls and writes one encoded
        // frame at a time and `close`s it when a write fails. It is a builtin
        // because the stream escapes the call that made it.
        b("serveStream")
            .sig(row("serveStream", &[], &[("s", Consume, stm(Str))], Unit, &[]))
            .spec(Spec::Traps)
            .effect(Serve)
            .hover("serveStream(stream) -> Unit — hand a `Stream<String>` of encoded frames to the serving host, which writes each one and closes the stream the first time a write fails; `std/http`'s `sse` is the one to call"),
        // The argument is an address, so nothing is consumed; the result's
        // stream type carries the disposal obligation.
        b("unboxStream")
            .sig(row("unboxStream", &["T"], &[("a", Read, Int)], stm(t()), &[]))
            .spec(Spec::Builds(stm(t())))
            .hover("unboxStream(address) -> Stream<T> — take a boxed stream back out; needs its type from the annotation: `let s: Stream<T> = unboxStream(a)`"),
        b("pullAt")
            .spec(Spec::Builds(opt(t())))
            .hover("pullAt(address) -> Option<T> — one element from the stream in that box; needs its type from the annotation: `let x: Option<T> = pullAt(a)`"),
        b("@pull").spec(Spec::Pulls),
        // The parts' lengths add up to the growth, which no one operand states.
        b("@strAppend")
            .spec(Spec::Rebuilds)
            .resizes(Length::Unknown, Elements::KeepsEachPosition),
        // String `a + b` and interpolation: copies both and allocates.
        b("@concat").sig(row(
            "@concat",
            &[],
            &[("a", Read, Str), ("b", Read, Str)],
            Str,
            &[],
        )),
        b("print")
            .sig(bounded(
                row("print", &["T"], &[("x", Read, t())], Unit, &[]),
                "T",
                crate::types::SHOW,
            ))
            .spec(Spec::Renders(Unit))
            .effect(WriteOutput),
        // Every allocating builtin needs a row, or an unannotated binding to its
        // result has no type and leaks. `toJson`'s parameter is a union: inert.
        b("toJson").sig(row("toJson", &[], &[("x", Read, Unit)], Str, &[])),
        // The generator is a name, not a value, and `x` any type: both inert.
        b("derive").sig(row(
            "derive",
            &[],
            &[("g", Read, Unit), ("x", Read, Unit)],
            Str,
            &[],
        )),
        // Both fold to literals, whose release is a no-op (`cap == 0`); the
        // rows exist so the declared reading names the type at every site.
        b("jsonSchema").sig(row("jsonSchema", &["T"], &[], Str, &[])),
        b("schemaOf").sig(row(
            "schemaOf",
            &["T"],
            &[],
            Type::Named("Schema".to_string()),
            &[],
        )),
        b("fromJson").sig(bounded(
            row(
                "fromJson",
                &["T"],
                &[("s", Read, Str)],
                Type::App("Validation".to_string(), vec![t()]),
                &[],
            ),
            "T",
            DECODABLE,
        )),
        // I/O: every result, error half included, is the caller's. Each routes
        // to a typed function over `std/runtime`'s untyped bodies; a generator
        // host reads a resource through the loader's resolver instead of WASI.
        b("args")
            .sig(row("args", &[], &[], arr(Str), &[]))
            .route("runtime$argsV")
            .effect(Args),
        b("readLine")
            .sig(row("readLine", &[], &[], opt(Str), &[]))
            .route("runtime$readLineV")
            .effect(ReadInput),
        b("parse")
            .sig(row("parse", &[], &[("s", Read, Str)], opt(Int), &[]))
            .route("runtime$parseV"),
        b("readFile")
            .sig(row(
                "readFile",
                &[],
                &[("p", Read, Str)],
                Type::result(Str, Str),
                &[],
            ))
            .route("runtime$readFileV")
            .gen_route("runtime$readFileGenV")
            .effect(FsRead),
        b("readFileBytes")
            .sig(row(
                "readFileBytes",
                &[],
                &[("p", Read, Str)],
                Type::result(arr(u8_()), Str),
                &[],
            ))
            .route("runtime$readFileBytesV")
            .gen_route("runtime$readFileBytesGenV")
            .effect(FsRead),
        b("writeFile")
            .sig(row(
                "writeFile",
                &[],
                &[("p", Read, Str), ("s", Read, Str)],
                Type::result(Bool, Str),
                &[],
            ))
            .route("runtime$writeFileV")
            .effect(FsWrite),
        b("writeFileBytes")
            .sig(row(
                "writeFileBytes",
                &[],
                &[("p", Read, Str), ("b", Read, arr(u8_()))],
                Type::result(Bool, Str),
                &[],
            ))
            .route("runtime$writeFileBytesV")
            .effect(FsWrite),
        b("writeStdout")
            .sig(row("writeStdout", &[], &[("b", Read, arr(u8_()))], Unit, &[]))
            .route("runtime$writeStdoutV")
            .effect(WriteOutput),
        b("renameFile")
            .sig(row(
                "renameFile",
                &[],
                &[("from", Read, Str), ("to", Read, Str)],
                Type::result(Bool, Str),
                &[],
            ))
            .route("runtime$renameFileV")
            .effect(FsWrite),
        b("fsyncFile")
            .sig(row(
                "fsyncFile",
                &[],
                &[("p", Read, Str)],
                Type::result(Bool, Str),
                &[],
            ))
            .route("runtime$fsyncFileV")
            .effect(FsWrite),
        // Generation-time calls; the rows name their types for the declared
        // reading inside a `gen fn`. `listDirKinds` is `listDir` with a
        // trailing `/` on each directory entry.
        b("listDir")
            .sig(row(
                "listDir",
                &[],
                &[("p", Read, Str)],
                Type::result(arr(Str), Str),
                &[],
            ))
            .route("runtime$listDirV")
            .gen_route("runtime$listDirGenV")
            .effect(FsList),
        b("listDirKinds")
            .sig(row(
                "listDirKinds",
                &[],
                &[("p", Read, Str)],
                Type::result(arr(Str), Str),
                &[],
            ))
            .route("runtime$listDirKindsV")
            .gen_route("runtime$listDirKindsGenV")
            .effect(FsList),
        b("moduleInterface")
            .sig(row(
                "moduleInterface",
                &[],
                &[("p", Read, Str)],
                Type::Named("ModuleInterface".to_string()),
                &[],
            ))
            .spec(Spec::Routes(crate::checker::GEN_ENTRY_MODULE_INTERFACE))
            .effect(GenOnly),
        // The argument is a contract name, not a value: inert. The checker
        // refuses anything but a declared contract name.
        b("contractOf")
            .sig(row(
                "contractOf",
                &[],
                &[("c", Read, Unit)],
                Type::Named("ContractInfo".to_string()),
                &[],
            ))
            .effect(GenOnly),
        b("lex")
            .spec(Spec::Routes(crate::checker::GEN_ENTRY_LEX))
            .effect(GenOnly),
        b("render").spec(Spec::Host).effect(GenOnly),
        b("raw").spec(Spec::Host).effect(GenOnly),
        b("rawAt").spec(Spec::Host).effect(GenOnly),
        b("@codeText").spec(Spec::Host).effect(GenOnly),
        b("@codeSplice").spec(Spec::Host).effect(GenOnly),
        b(crate::checker::GEN_REFLECT).spec(Spec::Host),
        b(crate::checker::GEN_NEXT_INT).spec(Spec::Host),
        b(crate::checker::GEN_NEXT_STR).spec(Spec::Host),
    ]);
    rows
}

/// Every builtin, in [`table`]'s order.
pub fn builtins() -> &'static [Builtin] {
    static ROWS: OnceLock<Vec<Builtin>> = OnceLock::new();
    ROWS.get_or_init(table)
}

/// The row a call site's name keys, in constant time: every call expression
/// asks.
pub fn builtin(name: &str) -> Option<&'static Builtin> {
    static BY_NAME: OnceLock<std::collections::HashMap<&'static str, usize>> = OnceLock::new();
    let at = BY_NAME.get_or_init(|| {
        builtins()
            .iter()
            .enumerate()
            .map(|(i, b)| (b.name, i))
            .collect()
    });
    at.get(name).map(|i| &builtins()[*i])
}

/// Whether `name` shrinks its receiver in place and hands back what it took.
pub fn removes(name: &str) -> bool {
    builtin(name).is_some_and(|b| b.spec == Some(Spec::Removes))
}

/// Returns the internal name `recv.name(..)` defaults to, if any.
pub fn method_builtin(name: &str) -> Option<&'static str> {
    builtins()
        .iter()
        .find(|b| b.method == Some(name))
        .map(|b| b.name)
}

/// Returns the surface spelling of an internal method-builtin name (`@push` to
/// `push`) for a diagnostic; any other name is returned unchanged.
pub fn method_surface(internal: &str) -> &str {
    builtin(internal).and_then(|b| b.method).unwrap_or(internal)
}

/// Every builtin's contract, in the table's order.
pub fn all<'a>() -> impl Iterator<Item = &'a Function> {
    builtins().iter().filter_map(|b| b.sig.as_ref())
}

/// What a builtin's specification row states about its operands and its
/// result. A builtin with such a row is a `call`, not a gap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Spec {
    /// Each operand at the stated type, in order, and a result at the stated
    /// type. The emitter writes one instruction between them.
    Typed(Vec<Type>, Type),
    /// One operand, at whatever type the row put on the name it reads, and a
    /// result of that same type.
    OwnType,
    /// One operand at its own type, handed back as the same value, not a
    /// copy, behind an optimization barrier (`blackBox`).
    Barrier,
    /// One operand at the type the row put on its name, and a result at the
    /// stated type (`print`, `@str`). The checker types the operand over a
    /// union; the emitter picks the rendering by the operand's type.
    Renders(Type),
    /// A message at `String`, and for `@panicAt` the site as a string
    /// literal. The call never returns; the `St::Trap` after it ends the
    /// path. `serveStream`'s message is the frontend's sentence, a literal in
    /// place of the stream, which a compiled build never pulls.
    Traps,
    /// `assert(c)` and `assertEq(a, b)`: a `Bool`, or two operands
    /// at one scalar type. A failure writes the interpreter's line and traps;
    /// the result is `Unit`.
    Asserts,
    /// A receiver first, rebuilt in place by the runtime; the store after the
    /// call puts the result back into the name.
    ///
    /// An array receiver takes at most one operand at its name's type; the
    /// result is the receiver's own storage. `@strAppend`'s receiver is a
    /// String accumulator (`NameInfo::grows`): the runtime grows the buffer
    /// when the ownership word says this path allocated it, and copies it
    /// otherwise. `@tally` takes a map, a key at its key type and an `Int64`
    /// count; it reads the key and never takes it (a miss stores a copy), so
    /// the caller releases its key on both paths. `@tallyBytes` reads bytes
    /// instead, and a miss stores a String built from them.
    Rebuilds,
    /// A SIMD operation. The vector operand's type chooses the
    /// instruction; a lane index is a literal the checker proved in range.
    Lanes,
    /// A host import a compiled generator calls:
    /// `@codeText`, `raw`, `rawAt` and `@codeSplice` answer a `Code` handle,
    /// `render` answers a String, and `__vyrnGen*` move a value out of the
    /// host as atoms. `@codeSplice`'s tag is its operand's type. Only a
    /// generator reaches one.
    Host,
    /// A call to the named function, which the program links: an entry a
    /// generator host synthesizes, or a `std` function a builtin routes to
    /// ([`Builtin::route`]). The emitter reads it as a declared callee.
    Routes(&'static str),
    /// A receiver the call shrinks in its own storage. `@pop` takes an array
    /// and hands back an `Option` of the last element, `@swapRemove` an
    /// array and an index at `Int64` and hands back the element there.
    /// `@remove` takes a map and a key at the map's key type, releases the
    /// key and value the entry held, and answers whether it held one.
    Removes,
    /// A map and a key at the map's key type. The answer is a `Bool`: whether
    /// the map holds an entry for the key.
    Finds,
    /// Operands at their names' types, and a result the call builds in its
    /// own storage, at the stated type with parameters the operands solve.
    Builds(Type),
    /// One operand at its name's type, and a result at the stated type. The
    /// call releases it (`close`), or boxes it and answers the box's address
    /// (`boxStream`).
    Effect(Type),
    /// The logging facade. `logger(name)` hands its String back as the
    /// `Logger`. A level takes a `Logger` and a message and writes the line,
    /// or nothing below the build's threshold; both operands are evaluated
    /// either way.
    Logs,
    /// The head of a `for` over a stream: advances the receiver in place and
    /// answers whether an element came. A read of the stream at the call's
    /// own name is that element.
    Pulls,
}

/// Returns the seeded row for the name a call site carries. The row's own
/// name `at` reaches it too, under the key [`crate::project::AT`].
pub fn signature(name: &str) -> Option<&'static Function> {
    let name = if name == "at" {
        crate::project::AT
    } else {
        name
    };
    builtin(name)?.sig.as_ref()
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
        .filter(|f| !lends(&f.name) && !matches!(f.ret, Type::Param(_)))
        .map(|f| (f.name.as_str(), &f.ret))
}

/// The capability parameter `i` of `name` declares.
pub fn capability(name: &str, i: usize) -> Option<Capability> {
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
    fn the_prelude_declares_eighteen_types_and_nothing_else() {
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
                "TypeNode",
                "TypeMember",
                "TypeArg",
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

    /// A pass that removes a bounds check reads the length effect; an array
    /// row that resizes and states none makes it forget every length.
    #[test]
    fn every_array_resizing_row_states_its_length_effect() {
        for b in builtins() {
            let resizes = matches!(b.spec, Some(Spec::Rebuilds | Spec::Removes))
                && b.sig
                    .as_ref()
                    .and_then(|f| f.params.first())
                    .is_some_and(|p| matches!(p.ty, Type::Array(_)));
            assert_eq!(
                resizes,
                b.length != Length::Unknown,
                "`{}` resizes an array: {resizes}",
                b.name
            );
        }
    }

    #[test]
    fn exactly_two_rows_are_views() {
        let views: Vec<&str> = all()
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
        for held in ["at", "atSet", "blackBox", "@swapRemove"] {
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
