//! The rules the compiler states, one row each: the rule, the holes its
//! sentence names, the sentence, and the fixes `vyrn fix` reads under it. The
//! lexer, parser, loader and checker state the first rows; the ownership and
//! typed judgments in `vyrn-lower` state the rows after them. A
//! [`Diagnostic`](crate::diagnostics::Diagnostic) built from a [`Rule`]
//! carries it, and its message is [`Rule::render`].

use crate::ast::{Speech, Spellings, Type};
use crate::diagnostics::menu;
use crate::types::{FALLIBLE, SHOW, SHOW_SHOW};

const HINT: &str = "generators run at compile time — they may not use `extern`, \
                    module state, `print`, `writeFile`, `readLine`, `args`, `readFileBytes`, \
                    the clock, entropy, or logging sinks";

macro_rules! rules {
    ($($rule:ident { $($hole:ident),* } $text:literal $(fix $fix:literal)*;)*) => {
        /// A rule the compiler states, with each hole's value.
        #[derive(Debug, Clone)]
        pub enum Rule {
            $($rule { $($hole: Hole),* },)*
        }

        impl Rule {
            /// Renders the sentence by linked names, then one fix line per way
            /// out.
            pub fn render(&self) -> String {
                self.render_in(&Spellings::default().speech(&None))
            }

            /// Renders the sentence as `speech` spells it, then one fix line
            /// per way out.
            // A rule without holes reads no speech.
            #[allow(unused_variables)]
            pub fn render_in(&self, speech: &Speech) -> String {
                match self {
                    $(Rule::$rule { $($hole),* } => {
                        let speech = Hole::sentence(speech, &[$($hole),*]);
                        $(let $hole = $hole.said(&speech);)*
                        menu(format!($text), Vec::<String>::from([$(format!($fix)),*]))
                    })*
                }
            }
        }
    };
}

/// A rule hole's value. A type and a declaration keep their linked names
/// until the sentence renders, which spells them ([`Speech`]). `Parts` is one
/// quoted form made of several, such as an impl head.
#[derive(Debug, Clone)]
pub enum Hole {
    Text(String),
    Type(Type),
    Decl(String),
    Parts(Vec<Hole>),
}

/// A declaration's linked name, to fill a rule hole ([`Hole::Decl`]).
pub struct DeclName<'a>(pub &'a str);

impl Hole {
    fn sentence<'a>(speech: &Speech<'a>, holes: &[&Hole]) -> Speech<'a> {
        fn collect<'h>(h: &'h Hole, types: &mut Vec<&'h Type>, names: &mut Vec<&'h str>) {
            match h {
                Hole::Text(_) => {}
                Hole::Type(t) => types.push(t),
                Hole::Decl(n) => names.push(n.as_str()),
                Hole::Parts(ps) => ps.iter().for_each(|p| collect(p, types, names)),
            }
        }
        let (mut types, mut names) = (Vec::new(), Vec::new());
        for h in holes {
            collect(h, &mut types, &mut names);
        }
        speech.sentence(&types, &names)
    }

    fn said(&self, speech: &Speech) -> String {
        match self {
            Hole::Text(s) => s.clone(),
            Hole::Type(t) => speech.ty(t).to_string(),
            Hole::Decl(n) => speech.name(n),
            Hole::Parts(ps) => ps.iter().map(|p| p.said(speech)).collect(),
        }
    }
}

/// What fills a rule hole: text through `Display`, a [`Type`] or a
/// [`DeclName`] as itself.
pub trait IntoHole {
    fn hole(&self) -> Hole;
}

macro_rules! text_holes {
    ($($t:ty),*) => {
        $(impl IntoHole for $t {
            fn hole(&self) -> Hole {
                Hole::Text(self.to_string())
            }
        })*
    };
}
text_holes!(
    String,
    str,
    usize,
    u32,
    u64,
    i64,
    char,
    crate::consteval::ConstVal,
    crate::artifacts::Target
);

impl<T: IntoHole + ?Sized> IntoHole for &T {
    fn hole(&self) -> Hole {
        (**self).hole()
    }
}

impl<T: IntoHole + ?Sized> IntoHole for Box<T> {
    fn hole(&self) -> Hole {
        (**self).hole()
    }
}

impl IntoHole for Type {
    fn hole(&self) -> Hole {
        Hole::Type(self.clone())
    }
}

impl IntoHole for Hole {
    fn hole(&self) -> Hole {
        self.clone()
    }
}

impl IntoHole for DeclName<'_> {
    fn hole(&self) -> Hole {
        Hole::Decl(self.0.to_string())
    }
}

/// Builds `Rule::$rule`. A hole is filled by the variable of its name or by
/// `hole = expr`, either through [`IntoHole`].
#[macro_export]
macro_rules! rule {
    (@hole $h:ident) => {
        $crate::rules::IntoHole::hole(&$h)
    };
    (@hole $h:ident $e:expr) => {
        $crate::rules::IntoHole::hole(&$e)
    };
    ($rule:ident $(, $h:ident $(= $e:expr)?)* $(,)?) => {
        $crate::rules::Rule::$rule { $($h: $crate::rules::rule!(@hole $h $($e)?)),* }
    };
}
pub use rule;

/// Builds the error that states `Rule::$rule` for `$stage` at `($line, $col)`.
macro_rules! refuse {
    ($stage:expr, $line:expr, $col:expr, $($rule:tt)*) => {
        $crate::diagnostics::Diagnostic::refusal($line, $col, $stage, $crate::rules::rule!($($rule)*))
    };
}
pub(crate) use refuse;

rules! {
    RedefinesBuiltinType { name } "cannot redefine built-in type `{name}`";
    TypeDefinedTwice { name } "type `{name}` defined twice";
    ReservedName { name } "`{name}` is a reserved name";
    EnumVariantDefinedTwice { name } "enum variant `{name}` is defined twice";
    VariantClashesWithType { name, from }
        "enum variant `{name}` clashes with the type `{name}`{from}; rename the variant";
    FunctionIsVariant { name } "`{name}` is both a function and an enum variant";
    FunctionDefinedTwice { name } "function `{name}` defined twice";
    FunctionIsType { name } "`{name}` is both a type and a function name";
    FunctionShadowsProtocolMethod { name, owners }
        "`{name}` collides with protocol {owners}'s method of the same name — \
        method names dispatch to impls before free functions, so this \
        declaration could never run";
    ImplMissesAssociatedType { protocol, ty, name }
        "`impl {protocol} for {ty}` does not bind the associated type `{name}` \
        — add `type {name} = ..` (protocol `{protocol}` declares it)";
    ImplBindsUndeclaredType { protocol, ty, name }
        "`impl {protocol} for {ty}` binds `type {name}`, which protocol `{protocol}` \
        does not declare";
    ImplMissesProjection { protocol, ty, want }
        "`impl {protocol} for {ty}` does not provide the projection `{want}`, \
        which protocol `{protocol}` declares — a protocol's members are \
        all required";
    ProjectionMismatchesProtocol { name, protocol, want, got }
        "projection `{name}` does not match protocol `{protocol}` — it declares \
        `{want}`, this provides `{got}`";
    ImplMissesMethod { protocol, ty, want }
        "`impl {protocol} for {ty}` does not provide `{want}`, which protocol `{protocol}` \
        declares — a protocol's methods are all required, so anything \
        holding a `T: {protocol}` may call it";
    MethodMismatchesProtocol { head, protocol, want, got }
        "`{head}` does not match protocol `{protocol}` — it declares `{want}`, this \
        provides `{got}`";
    ImplMethodUndeclared { head, sig, protocol, fix }
        "`{head}` provides `{sig}`, which protocol `{protocol}` does not declare — \
        dispatch knows only a protocol's own method names, so this one is \
        reachable from nowhere; {fix}";
    ImplUnknownProtocol { protocol, ty }
        "`impl {protocol} for {ty}`: there is no protocol named `{protocol}` — declare it with \
        `protocol {protocol} {{ .. }}` or import it";
    ImplHeadCollides { head, prev, prev_line, key, protocol }
        "`{head}` collides with `{prev}` (line {prev_line}) — Vyrn \
        dispatches on the type constructor, so `{key}` may have only one \
        impl of `{protocol}`; write one generic impl (`impl<T> {protocol} for {key}<T>`) to \
        cover every instantiation";
    ImplUnsupported { protocol, ty, why } "`impl {protocol} for {ty}` is not supported — {why}";
    NoMain {} "no `main` function found";
    MainSignature {} "`main` must have signature `fn main() -> Int64`";
    ExternTakesFn {}
        "an `extern` function may not take a `fn`-typed \
        parameter";
    GenTakesFn {} "a `gen fn` may not take a `fn`-typed parameter in v1";
    ExternReturnsFn {}
        "an `extern` function may not return a function value \
        (closures do not cross the host boundary)";
    GenReturnsFn {} "a `gen fn` may not return a function value";
    ProjectionOneReturn { name }
        "projection `{name}` must end with exactly one `return <place>` — \
        a projection is inlined at the access site, so it has one exit";
    ProjectionUsesTry { name }
        "projection `{name}` uses `?`, which returns — a projection is \
        inlined at the access site, so there is no frame to return \
        from. Check the condition and `panic` instead.";
    ProjectionReturnsValue { name }
        "projection `{name}` returns a value, not a place — a `read`/`modify` \
        result must be a field or element of `self`; a projection that \
        computes a new value is an ordinary `fn` returning `-> T`";
    OptionalProjectionSugarName { name }
        "`{name}` is dispatched by sugar that consumes a place unconditionally \
        — an optional projection needs a name of its own";
    OptionalProjectionReadSelf { name }
        "optional projection `{name}` must take `read self` — the hit is a \
        borrow the `if let` arm reads, and nothing writes through a miss";
    OptionalProjectionShape { name }
        "optional projection `{name}` must hold one `if <miss> {{ return None }}` \
        and end with `return Some(<place>)` — one prologue, one decision, \
        and statements after the decision run only on the hit";
    OptionalProjectionReturnsValue { name }
        "optional projection `{name}` answers `Some` of a value, not of a place \
        — the hit must be a field or element of `self`; a computed value \
        is an ordinary `fn` returning `-> Option<T>`";
    ProjectionForeignRoot { name, root }
        "projection `{name}` returns a place rooted at `{root}`, which the \
        access site does not own — a projection may only return a place \
        inside `self`, a parameter, or a prologue `let` that borrows \
        from one";
    DuplicateBlockName { noun, name, prev }
        "duplicate {noun} name {name:?} (already declared on line {prev})";
    MapKeyFloat { key }
        "`{key}` holds a float, and a `Map` key must hash and compare by equality: `NaN != NaN`, and `+0.0 == -0.0` would hash apart";
    MapKeyPayloadEnum { key }
        "`{key}` has a variant with a payload, and a payload-bearing enum key waits for real demand — a fieldless enum or a record of scalars keys today";
    MapKeyOwnsHeap { key }
        "`{key}` owns heap somewhere in its fields, and a `Map` key is stored, compared and freed by the map — a key must be heapless all the way down";
    ChainedProjectionUnresolved {}
        "the inner projection's result is not a concrete named type \
        here, so no engine can resolve the chain — bind the inner \
        access with a `let`, then read through the binding";
    OptionalPlaceTested { name }
        "an optional place is tested for its hit — write \
        `if let Some(x) = ..{name}(..)`; the miss is the `else` arm";
    OptionalPlaceRead { method }
        "an optional place is read where it is tested — write \
        `if let Some(x) = ..{method}(..)`, or reach for the copying \
        reader when the value must outlive the test";
    ProjectionArity { name, want, got }
        "projection `{name}` expects {want} argument(s) besides `self`, got {got}";
    ProjectionArgType { name, got, want } "projection `{name}` argument is {got}, expected {want}";
    ConstFailsPredicate { cv, name, pred }
        "{cv} does not satisfy `{name}` (predicate `where {pred}` is false)";
    ValueFailsPredicate { name, pred }
        "this value does not satisfy `{name}` (predicate `where {pred}` is false)";
    InterpolationFailsPredicate { witness, name, pred }
        "\"{witness}\" (a possible value of this interpolation) \
        does not satisfy `{name}` (predicate `where {pred}` is false)";
    StreamStored { ty, where_ }
        "`{ty}` may not be {where_} — a stream's lifetime is a scope, \
        so it may be a binding, a parameter, or a return type, and nothing may \
        store it";
    TypeHoldsStream { ty }
        "`{ty}` holds a `Stream`, but a stream's lifetime is a scope — \
        it may be a binding, a parameter, or a return type, and nothing may \
        store it";
    GenOnlyType { name } "the `{name}` type is only available during generation";
    SelfNotType {}
        "`Self` is not a type in Vyrn — a protocol that must name the \
        implementing type declares an associated type instead: `protocol P {{ type \
        Out  fn m(self) -> Out }}`, and each impl binds it with `type Out = ..`";
    UnknownType { n } "unknown type `{n}`";
    GenericNeedsArgs { n } "`{n}` is generic; write `{n}<...>` with type arguments";
    TypeTakesNoInteger { name } "type {name} does not take an integer argument";
    TypeArity { name, want, got } "`{name}` takes {want} type argument(s), got {got}";
    TransformerBaseNotRecord {} "the transformer's base must be a record type";
    TransformerFieldMissing { k } "field `{k}` is not in the transformer's base record";
    MergeNeedsRecords {} "`Merge` requires two record types";
    PartialNeedsRecord {} "`Partial` requires a record type";
    SmallArrayCapacity {} "smallArray capacity must be between 1 and 64";
    IntegerNotType {}
        "an integer is not a type; only `SmallArray<T, N>` \
        takes an integer argument";
    MapKeyNeedsHashable { key }
        "`{key}` can be a `Map` key once it declares the obligation: `impl Hashable for {key}` — equal values must return equal hashes";
    MapKeyType { key }
        "a `Map` key is `String`, `Int64`, or a heapless `Hashable` type, found `{key}`";
    FnTypeTakesFn {} "a function type may not take another function value";
    FnTypeReturnsFn {} "a function type may not return another function value";
    DefaultMismatch { where_, vty, ty } "{where_} defaults to {vty}, but is declared `{ty}`";
    FnTypeWhere {} "a function type cannot carry a `where` predicate";
    DuplicateField { field, record } "duplicate field `{field}` in record `{record}`";
    EnumWhere {} "an enum type cannot have a `where` clause";
    EnumEmpty { name } "enum `{name}` has no variants";
    AliasWhere { noun } "a `{noun}` alias cannot have a `where` clause";
    RecordWhere {} "a record type cannot have a `where` clause";
    NotRecord { name } "`{name}` does not resolve to a record";
    ValidatedBaseNotScalar { name }
        "`{name}` must have a scalar base (Int64, sized int, Float64, Bool, or String)";
    PredicateCalls { kind, name } "{kind} predicate for `{name}` may not contain calls (v0.1)";
    PredicateNotBool { kind, name, pty } "{kind} predicate for `{name}` must be Bool, found {pty}";
    ExternParamType { func, param, ty }
        "extern fn `{func}` parameter `{param}` has type {ty}, which cannot cross \
        the JS boundary (allowed: Int64, sized ints, Float64, Float32, Bool, String)";
    ExternReturnType { name, ret }
        "extern fn `{name}` returns {ret}, which cannot cross the JS boundary \
        (allowed: Int64, sized ints, Float64, Float32, Bool, String, Unit)";
    GlobalUnit { name } "cannot bind module state `{name}` to a Unit value";
    GlobalStream { name }
        "module state `{name}` may not be a `Stream` — a stream's \
        lifetime is a scope, and module state is never dropped";
    InitMismatch { name, declared, vty }
        "`{name}` declared {declared} but initializer is {vty}";
    RegionEscape { name }
        "cannot store a heap value into `{name}`, which \
        outlives the enclosing `region` (it would dangle when the \
        region frees). Move `{name}` inside the region, or compute a \
        non-heap result to carry out.";
    RegionConsume { arg, callee }
        "cannot hand a heap value to argument {arg} of `{callee}`, which is \
        `consume`, inside a `region`. The region frees the value at its closing brace, \
        so the callee cannot own it. Move the call out of the region, or pass a value \
        that holds no heap.";
    IntLiteralOverflow { n }
        "integer literal {n} exceeds Int64's maximum \
        (9223372036854775807); only `UInt64` can hold it — \
        annotate the binding (`let x: UInt64 = ...`)";
    InferNone {}
        "cannot infer the type of `None`; \
        add an annotation (e.g. `let x: Option<Int64> = None;`)";
    InferBinding { name } "cannot infer the type of `{name}`; add an annotation";
    ArrayLiteralNotArray { ty }
        "`[]` is an array literal, but {ty} is not an \
        array type";
    InferEmptyArray {}
        "cannot infer the element type of `[]`; annotate it, \
        e.g. `let a: Array<Int64> = [];`";
    ArrayElementMismatch { elem_ty, t }
        "array elements must share a type: expected {elem_ty}, found {t}";
    MapLiteralNotMap { ty } "`[:]` is a map literal, but {ty} is not a map type";
    InferEmptyMap {}
        "cannot infer the type of `[:]`; annotate it, \
        e.g. `let m: Map<String, Int64> = [:];`";
    MapLiteralKeyMismatch { key_ty, kt } "the map is keyed by {key_ty}, but this key is {kt}";
    MapValueMismatch { val_ty, vt }
        "map values must share a type: expected {val_ty}, \
        found {vt}";
    LambdaNeedsFnType {}
        "a lambda `|..|` needs a function type from context: \
        pass it to a `fn`-typed parameter, or give the binding a function \
        type (e.g. `let f: fn(Int64) -> Int64 = x -> x * 2`)";
    RecordFailsPredicate { name, pred } "`{name} {{ .. }}` violates `where {pred}`";
    InferRecordParam { tp, name, shape }
        "cannot infer type parameter `{tp}` of `{name}`; no field \
        value determines it, so annotate the binding (e.g. `let x: {name}<{shape}> = \
        {name} {{ .. }}`)";
    TryOutsideFn {} "`?` can only be used inside a function";
    TryOptionReturn { ret }
        "`?` on an Option requires the function to return Option, \
        but it returns {ret}";
    TryErrorMismatch { e, re }
        "`?` propagates error {e}, but the function returns \
        Result<_, {re}>";
    TryResultReturn { ret }
        "`?` on a Result requires the function to return Result, \
        but it returns {ret}";
    TryOperand { other }
        "`?` needs an Option, a Result, or a type that implements \
        `{FALLIBLE}`, found {other}";
    TryFallibleReturn { other, ret }
        "`?` propagates the whole {other}, but the function \
        returns {ret}";
    MatchScrutinee { form, sty }
        "`{form}` scrutinee must be an Option, Result, or enum, found {sty}";
    BlockArmAsValue {}
        "a `match` used as a value has single-expression arms; a block arm needs statement position";
    BlockArmOutsideFn {} "a block arm needs a function body around it";
    DefaultOperand { sty }
        "`??` works on an Option or a Result, not on {sty} — \
        `match` names the variant to fall back on";
    NotAVariant { vname, sty } "`{vname}` is not a variant of {sty}";
    PatternArity { vname, want, got }
        "variant `{vname}` has {want} payload(s), but the pattern binds {got}";
    IfNeedsElse {}
        "`if` used as an expression needs an `else` (every branch \
        must yield a value)";
    MatchArmMismatch { rt, bty } "`match` arms have differing types: {rt} vs {bty}";
    ParamOperand { t, r } "cannot combine type parameter `{t}` with {r}";
    ParamNeedsNum { t } "`{t}` needs a `Num` bound for arithmetic";
    ParamNeedsOrd { t } "`{t}` needs an `Ord` bound to compare";
    ParamNeedsEq { t } "`{t}` needs an `Eq` bound";
    ParamLogic {} "`&&`/`||` need Bool operands";
    ParamMatch { t } "`=~` needs a String operand, not `{t}`";
    ParamBitwise { t } "bitwise operators need a concrete integer type, not `{t}`";
    SimdIntDivide {}
        "`I32x4` has no `/` — no hardware has SIMD integer \
        divide, so there is no instruction to emit. Read the lanes out \
        and divide them, or use `F32x4`";
    ConcatOperands { l, r } "`+` concatenates two Strings, found {l} and {r}";
    ArithOperands { l, r }
        "arithmetic needs matching numeric operands, \
        found {l} and {r}";
    FloatRemainder { f } "no `%` on {f}; integer remainder only";
    RemainderOperands { l, r } "`%` needs matching integer operands, found {l} and {r}";
    CompareOperands { l, r }
        "comparison needs matching numeric or String operands, \
        found {l} and {r}";
    EqualityOperands { l, r } "`==`/`!=` needs matching scalar operands, found {l} and {r}";
    LogicOperands { l, r } "`&&`/`||` needs Bool operands, found {l} and {r}";
    BitwiseMismatch { l, r }
        "bitwise operators need matching integer operands, \
        found {l} and {r}";
    BitwiseOperands { l, r } "bitwise operators need integer operands, found {l} and {r}";
    MatchOperands { l, r } "`=~` needs a String and a pattern, found {l} and {r}";
    LaneType { what, lane, t } "{what} takes {lane} lanes, found {t}";
    LaneCount { what, lanes, got } "`{what}(..)` takes {lanes} lanes, got {got}";
    SplatArity { what, got } "`{what}.splat(..)` takes 1 argument, got {got}";
    LaneArity { got } "`lane` takes a vector and a lane index, got {got} argument(s)";
    LaneReceiver { other }
        "`lane` must be called on a vector or a mask \
        (e.g. `v.lane(0)`), found {other}";
    LaneIndex { max }
        "a lane index must be a compile-time constant in 0..{max} \
        (that is what makes `lane` total — there is no bounds check to fall back on)";
    ReplaceLaneArity { got }
        "`replaceLane` takes a lane index and a value \
        (it is `v.replaceLane(k, x)`), got {got} argument(s)";
    ReplaceLaneReceiver { v }
        "`replaceLane` must be called on a vector \
        (e.g. `v.replaceLane(0, x)`), found {v}";
    ReplaceLaneIndex { max }
        "a lane index must be a compile-time constant in 0..{max} \
        (that is what makes `replaceLane` total — there is no bounds check \
        to fall back on)";
    MaskArity { what, got }
        "`{what}` takes no arguments (it is `m.{what}()`), \
        got {got}";
    MaskReceiver { what, m }
        "`{what}` must be called on a mask \
        (e.g. `(a < b).{what}()`), found {m}";
    VectorOpArity { what, op, want, got } "`{what}.{op}(..)` takes {want} arguments, got {got}";
    VectorStoreTarget { what }
        "`{what}.store` needs an array binding as its first \
        argument, not an expression";
    VectorArrayType { what, op, lane, other } "`{what}.{op}` needs an Array<{lane}>, found {other}";
    VectorIndexType { i } "a vector load/store index must be an Int64, found {i}";
    VectorStoreValue { what, v } "`{what}.store` stores an {what}, found {v}";
    VectorOpType { ty, what, t } "`{ty}.{what}` takes {ty}, found {t}";
    VectorNoMethod { ty, m } "`{ty}` has no `{m}`";
    FnValueArity { name, want, got }
        "`{name}` is a function value taking {want} argument(s), got {got}";
    FnValueArgType { name, arg, pty, aty } "`{name}` argument {arg} expects {pty}, found {aty}";
    CallsLocal { name, ty } "`{name}` is a local of type {ty}, not a function"
        fix "rename the local to call the function `{name}`";
    TestOnly { name }
        "`{name}` is only available inside a `test` block — in ordinary \
        code, use a validated type or return a `Result` to signal failure";
    TakesTwo { name, got } "`{name}` takes 2 arguments, got {got}";
    AssertEqOperands { a, b } "`assertEq` needs two equal, equatable values, found {a} and {b}";
    BlackBoxOutsideBench {}
        "`blackBox` is only available inside a `bench` or `test` block — \
        it exists to defeat the optimizer while measuring, not for ordinary code";
    TakesOne { name, got } "`{name}` takes 1 argument, got {got}";
    PanicArity { got } "`panic` takes 1 String argument, got {got}";
    PanicType { t } "`panic` needs a String, found {t}";
    GenOnly { name } "`{name}` is only available during generation";
    GenOnlySurface { surface } "{surface} only available during generation";
    QuoteSplice { t }
        "cannot splice {t} into a code quote \
        (expected String, number, Bool, or Code)";
    RenderType { t } "`render` needs a Code value, found {t}";
    RawAtArity { got } "`rawAt` takes 4 arguments (text, path, line, col), got {got}";
    BytesArity { got } "`bytes` takes 1 argument, or 3 with a byte range, got {got}";
    BytesType { t } "`bytes` needs a String, found {t}";
    BytesOffsets { n } "`bytes` needs Int64 offsets, found {n}";
    PushReceiver { other } "`push` needs an Array as its first argument, found {other}";
    PushValue { v, elem } "`push` value is {v} but the array holds {elem}";
    MapKeyMismatch { key, k } "the map is keyed by {key}, but the key here is {k}";
    IndexReceiver { other } "indexing needs an Array or String, found {other}";
    IndexType { i } "`at` index must be an Int64, found {i}";
    StreamAddress { name, at } "`{name}` needs a boxed stream's address, found {at}";
    StreamElementContext { name, want }
        "`{name}` needs the element type from context — \
        write `let x: {want} = {name}(a)`";
    StreamElementMismatch { name, want, exp } "`{name}` answers a `{want}`, not {exp}";
    TakesNone { name } "`{name}` takes no arguments";
    SwapRemoveArity { got } "`swapRemove` takes 1 argument (an index), got {got}";
    SwapRemoveIndex { i } "`swapRemove` index must be an Int64, found {i}";
    ToArrayReceiver { other } "`toArray` needs a SmallArray, found {other}";
    CopyOwned { declared }
        "`copy` cannot copy `{declared}`: it declares `impl Owned for \
        {declared}`, so only `{declared}` knows what duplicating it means. Say what \
        duplicating it means with `impl Copy for {declared}`, or copy the parts you \
        need";
    CopyStream {}
        "`copy` cannot copy a Stream: a stream is a cursor over a \
        producer, not a container. Collect it first (`collect`), then copy the array";
    CopyRecursive { name }
        "`copy` cannot copy `{name}`: it refers to itself, so a \
        structural copy has no bottom to stop at. Write a recursive function that \
        copies it one variant at a time, and declare it with `impl Copy for {name}`";
    MapOpReceiver { op, other } "`{op}` needs a Map as its receiver, found {other}";
    MapOpArity { op } "`{op}` takes 1 argument (a key)";
    MapRemoveReceiver {} "`remove` needs a plain map variable as its receiver";
    ConversionArity { name, got } "`{name}` conversion takes 1 argument, got {got}";
    ConversionType { name, src } "`{name}(..)` converts a number, found {src}";
    TargetAsTypeArg { name, was, suffix }
        "`{name}` names its target as a type argument — write `{name}<{was}>({suffix})`";
    ContractOfArity { got } "`contractOf` takes 1 argument (a contract name), got {got}";
    ContractOfUnknown { cn }
        "`contractOf` needs a declared contract name; \
        `{cn}` is not a contract";
    ContractOfName {} "`contractOf` needs a contract name";
    ToJsonArity { got } "`toJson` takes 1 argument (a value), got {got}";
    ToJsonUncodable { off } "`toJson` cannot encode `{off}` (not a codable type)";
    DeriveArity {} "`derive` takes a generator's name and a value: `derive(g, x)`";
    DeriveUnknownGen { g } "`derive` needs a `gen fn {g}(t: TypeArg) -> String`; `{g}` is not one";
    DeriveUncodable { g, off } "`derive({g}, ..)` cannot reflect `{off}`: it has no wire form";
    DeriveEntryMismatch { g, got, want }
        "generator `{g}` wrote the entry `{got}`, and this call needs `{want}`";
    ValueType { t } "`value` boxes an Int64, Bool, or String, found {t}";
    ValueTypeNoShow { t, key }
        "`value` boxes an Int64, Bool, or String, found {t} \u{2014} say how it renders \
        with `impl {SHOW} for {key}`";
    ListType { other } "`@list` needs an Array, found {other}";
    SomePayload { aty, want } "`Some` payload is {aty} but Option<{want}> was expected";
    InferVariant { name }
        "cannot infer the type of `{name}(..)`; add an annotation \
        (e.g. `-> Result<Int64, Int64>`)";
    VariantPayload { name, aty, want_ty } "`{name}` payload is {aty} but {want_ty} was expected";
    VariantNoArgs { name } "variant `{name}` takes no arguments";
    VariantArity { name, want, got } "`{name}` takes {want} argument(s), got {got}";
    InferParam { tp, name } "cannot infer type parameter `{tp}` of `{name}`";
    NeedsReceiver { name } "`{name}` needs a `self` receiver";
    AmbiguousBound { name, protocols }
        "`{name}` is ambiguous: protocols {protocols} all declare it for this receiver";
    AmbiguousImpl { name, protocols }
        "`{name}` is ambiguous: protocols {protocols} all implement it for this receiver";
    BoundAssociatedType { name, proto, a, t }
        "`{name}` mentions `{proto}`'s associated type `{a}`, and \
        a `<{t}: {proto}>` bound cannot name it — call `.{name}(..)` on a \
        concrete type, where the impl (and so `{a}`) is known";
    NotImplemented { recv, proto, name }
        "{recv} does not implement protocol `{proto}` \
        (needed for `.{name}(..)`)";
    ProjectionOnBound { name }
        "`{name}` is a projection, and a projection inlines at its \
        access site — a `<T: ..>` receiver has no body to inline, \
        so call `.{name}(..)` on a concrete type";
    UnknownFunction { name } "call to unknown function `{name}`";
    BoundUnsatisfied { shown, tp, b, concrete }
        "`{shown}` requires `{tp}: {b}`, but {concrete} does not satisfy `{b}`";
    InferLambdaReturn {}
        "cannot infer the return type of a \
        block-bodied lambda passed to a generic `fn` parameter; \
        use an expression body `|..| expr`";
    ArgNotFn { callee, arg, vn }
        "`{callee}` argument {arg} expects a function; `{vn}` is \
        neither a lambda nor a known function";
    LambdaArity { got, exp, want }
        "this lambda takes {got} parameter(s), but the expected \
        function type `{exp}` takes {want}";
    FnValueUnsolved { name, exp, params, first }
        "`{name}` cannot be stored as `{exp}`: nothing has solved {params} \
        here, so there is no signature to check `{name}` against. Annotate the \
        binding with a type that names `{first}`";
    GenericFnValue { name }
        "`{name}` is generic and cannot be used as a function \
        value in v1";
    ExternFnValue {}
        "an `extern` function cannot be used as a function \
        value — the host boundary dispatches by name";
    GenFnValue {}
        "a `gen fn` runs at generation time and cannot be \
        used as a function value";
    FnValueCapability { name, param, cap }
        "`{name}` cannot be used as a function value: it takes \
        `{param}` by `{cap}`, and a `fn` type reads every argument";
    ModifyNotMut { fname, arg, vn }
        "`{fname}` argument {arg} is `modify`, so `{vn}` must be \
        declared `mut`";
    ModifyTemporary { fname, arg }
        "`{fname}` argument {arg} is `modify`; pass a mutable \
        variable, not a temporary";
    ModifyExactType { fname, arg, pty, aty }
        "`{fname}` argument {arg} is `modify` and needs exactly \
        {pty}, found {aty} (width subtyping is read-only: a wider \
        record could lose fields on write-back)";
    ParamConflict { t, bound, aty } "type parameter `{t}` is both {bound} and {aty}";
    ExpectedOption { aty } "expected Option, found {aty}";
    ExpectedResult { aty } "expected Result, found {aty}";
    Expected { pty, aty } "expected {pty}, found {aty}";
    ArgExpected { pty, aty } "argument expects {pty}, found {aty}";
    ArrayOpReceiver { op } "`{op}` needs a plain array variable as its receiver";
    GenImpure { name, reason } "`gen fn {name}` is not comptime-pure: it {reason} ({HINT})";
    GenImpureVia { name, cur, chain, reason }
        "`gen fn {name}` is not comptime-pure: it reaches `{cur}` (via \
        {chain}), which {reason} ({HINT})";
    GlobalReadsItself { own_name }
        "module state `{own_name}` may not read itself in its \
        own initializer";
    GlobalReadsLater { own_name, name }
        "initializer of `{own_name}` reads `{name}`, a module-state \
        binding declared later — a global may only read earlier ones";
    GlobalCalls { own_name, name }
        "initializer of `{own_name}` may not call `{name}` — a \
        module-state initializer runs before `main`, so it may use only \
        literals, operators, built-ins, and functions imported from another \
        module (whose state initializes first)";
    ShowReturnsString { ret }
        "`{SHOW}`'s `{SHOW_SHOW}` must hand back a String to render through, found {ret}";
    ExternConsume { func, param }
        "extern fn `{func}` parameter `{param}` may not be `consume` — the caller \
        across this boundary is JS, and it releases the String when the \
        call returns"
        fix "take `{param}: String` and store `{param}.copy()`";
    GoneModule { name, module }
        "`{name}` is `{module}`'s — add `import {{ {name} }} from \"{module}\"`";
    GoneRemoved { hint } "{hint}";
    GoneDesugared { name, module, sugar }
        "`{name}` is `{module}`'s, and `{sugar}` writes through it — add \
        `import {{ {name} }} from \"{module}\"`";
    NeedsShow { shown, found } "`{shown}` needs a number, Bool, or String, found {found}";
    NeedsShowImpl { shown, found, key }
        "`{shown}` needs a number, Bool, or String, found {found} \u{2014} say how it \
        renders with `impl {SHOW} for {key}`";
    LambdaAssignsCapture { name, line }
        "a lambda captures by read; it cannot assign to the captured \
        binding `{name}` (line {line})";
    LambdaMutatesCapture { name, line }
        "a lambda captures by read; it cannot mutate a field of the \
        captured binding `{name}` (line {line})";
    LambdaStoresIntoCapture { name, line }
        "a lambda captures by read; it cannot store into the captured \
        binding `{name}` (line {line})";
    LambdaDropsCapture { name, line }
        "a lambda cannot `drop` the captured binding `{name}` (line {line})";
    LambdaConsumesCapture { name, line }
        "a lambda cannot consume the captured binding `{name}` (line {line})";
    LambdaNestsLambda { line }
        "a lambda body may not contain another lambda literal in v1 (line {line})";
    ModifyAliased { path, fname, root }
        "`{path}` is passed to `{fname}` as `modify` and read again in the \
        same call — a `modify` borrow is exclusive"
        fix "`{root}.copy()` for the second argument"
        fix "or split the call so the two accesses do not overlap";
    NulInString {}
        "string literal contains a NUL byte; a Vyrn String is NUL-terminated and \
        cannot hold one";
    UnterminatedString {} "unterminated string literal";
    UnterminatedEscape {} "unterminated escape in string";
    UnterminatedInterpString {} "unterminated string in interpolation";
    UnterminatedInterpChar {} "unterminated character literal in interpolation";
    UnterminatedInterp {} "unterminated `\\{{` interpolation";
    EmptyInterp {} "empty `\\{{ }}` interpolation";
    UnknownEscape { other } "unknown escape `\\{other}`";
    InvalidFloat { text } "invalid float literal: {text}";
    IntOutOfRange { text } "integer literal out of range: {text}";
    UnexpectedChar { c } "unexpected character {c}";
    ExpectedIdent { found } "expected identifier, found {found}";
    DuplicateLogging {} "duplicate `logging` config block";
    TopLevelExpected { found }
        "expected `fn`, `type`, `protocol`, `contract`, `impl`, `let`, or \
        `logging` at top level, found {found}";
    AssocTypeRhs { aname }
        "`type {aname}` in a protocol declares an associated type and takes no \
        right-hand side — the implementing type supplies it, so \
        `type {aname} = ..` belongs in the `impl`";
    AssocTypeAfterMethods { aname, name }
        "`type {aname}` must be declared before the methods of `{name}` — an \
        associated type is resolved where it is named, so a signature above \
        it cannot see it";
    ProjectionReceiver { name, want }
        "`fn {name}` returns `{want} T`, so its receiver must be \
        `{want} self` — the result is a place inside the receiver, \
        and the two capabilities name one access";
    ContractMemberExpected { name, found }
        "expected `let` or `fn` in contract `{name}`, found {found} \
        (a contract member is `let name: Type [= default]`, \
        `fn name(..) -> T [= default]`, or the open rule `fn *(..) -> T`)";
    ContractOpenRuleTwice { name, line }
        "contract `{name}` already has an open rule (line {line}) — \
        a contract has at most one";
    ContractMemberFormChange { name, member, line }
        "contract `{name}` declares `{member}` as both a value and a function \
        (line {line}) — alternative signatures are alternatives, not a \
        change of member form";
    ContractMemberTwice { name, member, line }
        "contract `{name}` already declares `{member}` (line {line}) — only \
        `fn` members may have alternative signatures";
    ContractMemberNeedsType { name }
        "contract member `{name}` needs a type: write `let {name}: Type` \
        (a contract states the shape of an export, so the type is never inferred)";
    ContractMemberParams { name }
        "contract member `{name}` cannot take `(..)` — only the open rule \
        `fn *(..)` may leave its parameters open, because a named member's \
        arity is part of what the name promises";
    ContractOpenRuleDefault {}
        "a contract's open rule cannot have a default — it describes the shape of \
        exports whose names the contract does not know, so there is no absent \
        member for a default to supply";
    ImplAssocTypeOrder { aname, protocol, ty }
        "`type {aname} = ..` must be declared before the methods of \
        `impl {protocol} for {ty}` — an associated type is resolved where it \
        is named, so a method above it cannot see it";
    ImplAssocTypeClash { aname }
        "`type {aname}` collides with the `{aname}` this impl's head binds — \
        an associated type and a type variable are different things and \
        cannot share a name";
    ConsumeResult {}
        "a result is owned by its caller already — `-> consume T` is spelled `-> T`";
    UnknownLogLevel { name } "unknown log level `{name}` (trace/debug/info/warn/error)";
    UnknownLoggingField { other }
        "unknown `logging` field `{other}` (expected `level` or `sink`)";
    FileSinkPath { found } "`file(..)` sink needs a string path, found {found}";
    UnknownSink { other } "unknown sink `{other}` (expected stderr, stdout, or file(\"..\"))";
    ImportStarAs { found } "expected `as` after `import *`, found {found}";
    ImportStarFrom { ns, found } "expected `from` after `import * as {ns}`, found {found}";
    ImportEmpty {} "an import must name at least one binding: `import {{ name }} from \"..\"`";
    ImportFrom { found } "expected `from` after the import list, found {found}";
    ImportPathExpected { found }
        "expected a module path string or a generator call after `from`, \
        found {found}";
    InlineWhereBase { name }
        "an inline field `where` refines one record's fields, so the base of \
        `type {name}` must be exactly `{{ .. }}` — a merge (`&`) or enum \
        variant cannot carry refinements";
    LazyAnonymous {}
        "a `lazy` field needs a named record type \
        (`type T = {{ field: lazy U }}`); an anonymous record \
        has no declaration to defer against";
    WhereAnonymous {}
        "an inline field `where` needs a named record type \
        (`type T = {{ field: .. where .. }}`); an anonymous record \
        has no name to attach the refinement to";
    LazyWhere {}
        "a `lazy` field may not carry an inline `where`: name the \
        validated type and defer that (`field: lazy Body`)";
    FreeFnCapability { name }
        "`fn {name}` cannot return a capability — a projection's result \
        is a place inside its receiver, and a free function has none. \
        Declare it on an `impl`.";
    NameStringExpected { word, found } "expected a {word} name string, found {found}";
    ExportedExternBody {}
        "an exported extern needs a body — a body-less `extern fn` is an import";
    ExternBody {} "an `extern fn` has no body";
    IntUnsized {}
        "`Int` has no size; write `Int64` (or `Int8`/`Int16`/`Int32`, \
        `UInt8`..`UInt64`)";
    FloatUnsized {} "`Float` has no size; write `Float64` (or `Float32`)";
    ArraySize {} "`Array<T, N>` needs a non-negative integer size";
    SmallArrayNeedsCapacity {}
        "`SmallArray<T, N>` needs an inline capacity, e.g. \
        `SmallArray<Int64, 16>`";
    SmallArrayCapacityType {} "`SmallArray<T, N>` needs a non-negative integer capacity";
    NeedsField { name } "`{name}` needs at least one field, e.g. `{name}<T, field>`";
    NestingTooDeep { max } "nesting exceeds {max} levels";
    GlobalNeedsInit { name }
        "module state `{name}` needs an initializer: write `let {name} = <value>` \
        (top-level `let` has no default value)";
    LetMutPattern { variant }
        "`let mut {variant}(..)` — a pattern binder is a borrow of the \
        scrutinee's payload; bind it, then `copy()` what you mutate";
    LetPatternScrutinee { variant }
        "the scrutinee of `let {variant}(..)` must be a name — a \
        multi-payload pattern reads it once per binder, so bind the \
        value with an ordinary `let` first";
    LetEmptyVariant { variant }
        "`let {variant}()` binds nothing — a payload-free variant is a \
        question, and `if let`/`match` are how it is asked";
    IndexAssignTarget {}
        "the left side of an index assignment `[i] = ..` must be \
        an array variable, a record field, or an array element";
    FieldAssignTarget {}
        "the left side of `[i].field = ..` must be an array \
        variable, a record field, or an array element";
    FieldWriteDepth {}
        "only a single field write-through is supported: \
        `a[i].field = v` (not `a[i].field.field = v`)";
    PushNoPlace {}
        "this `push` has no place to write back to, so it \
        would silently do nothing. Its receiver must be an \
        assignable place: a variable (`xs.push(v)`), a \
        record field (`r.xs.push(v)`), or an array element \
        (`a[i].push(v)`) — not a temporary or a deeper chain.";
    TemplateNoHole { name }
        "a tagged template `{name}\"..\"` needs at least one `\\{{ }}` \
        interpolation; use a plain string otherwise";
    UnexpectedToken { found } "unexpected token in expression: {found}";
    InInterpolation { detail } "in interpolation: {detail}";
    InterpolationTrailing { src } "unexpected tokens after interpolation expression `{src}`";
    IfLetExpr {}
        "`if let` is a statement, not an expression — use `match` to bind \
        a pattern in an expression position";
    IfExprStatements {}
        "an `if` used as an expression takes a single expression in each \
        branch, not statements — use the statement form or a function for \
        multi-statement branches";
    FloorCannotInclude { artifact, target, module, what }
        "artifact `{artifact}` ({target}) cannot include `{module}`: {what}";
    FormatterInvariant {}
        "internal formatter error: output would change the token sequence \
        (source left unchanged)";
    DeriveWroteDerive {} "a `derive` generator wrote a `derive` call";
    AudienceCannotImport { importer, from, imported, to }
        "`{importer}` is {from} and cannot import `{imported}`, which is {to}";
    AudienceRuntime { shown, fenced }
        "`{shown}` cannot import `{fenced}`, whose audience is the runtime";
    GenImportsDeep { max }
        "generator imports nest more than {max} deep — a generator \
        likely imports itself with a growing argument";
    ImportCycle { cycle, key } "import cycle: {cycle} -> {key}";
    CannotLoad { key, why } "cannot load `{key}`: {why}";
    LoggingRootOnly { key } "`{key}`: only the root module may configure `logging {{ .. }}`";
    DeclaredByBoth { name, first, second }
        "`{name}` is declared by both `{first}` and `{second}`";
    DeclaredByBothLinked { name, first, second }
        "`{name}` is declared by both `{first}` and `{second}` — a top-level name is \
        program-wide, so two linked modules cannot share one";
    SourceTooLong { src_len } "src_len {src_len} exceeds the input buffer";
    NotUtf8 {} "the source is not valid UTF-8";
    UnicodeEscapeBrace {} "`\\u` must be followed by `{{HEX}}`";
    UnterminatedUnicodeEscape {} "unterminated `\\u{{` escape";
    UnicodeEscapeDigits {} "`\\u{{}}` needs hex digits";
    UnicodeEscapeScalar {} "invalid Unicode scalar in `\\u{{}}`";
    UnterminatedByte {} "unterminated byte literal";
    EmptyByte {}
        "empty byte literal; a byte literal holds exactly one byte, e.g. 'a' or '\\x0a'";
    UnterminatedByteEscape {} "unterminated byte escape";
    ByteHexDigits {} "`\\x` needs two hex digits";
    UnknownByteEscape { other } "unknown byte escape `\\{other}`";
    ByteNewline {} "raw newline in byte literal; write '\\n'";
    ByteNotAscii {}
        "byte literal must be a single ASCII byte; write the UTF-8 bytes explicitly";
    SingleQuotedString {}
        "single-quoted strings are not allowed: '…' is a single byte \
        (e.g. 'a', '\\n', '\\x41'); use \"…\" for text";
    ModuleStateExport {}
        "module state is not exportable — export accessor functions \
        (a top-level `let` is module-private in every module)";
    ExportNeedsDecl {}
        "`export` must be followed by `fn`, `type`, `protocol`, `contract`, \
        `extern fn`, `gen fn`, or `mut fn`";
    LambdaNotHere { form, fix }
        "`{form} ...` is not a lambda here; a lambda takes its parameters before an arrow"
        fix "{fix}";
    SkeletonDetail { detail } "`vyrn\"…\"` skeleton does not parse: {detail}";
    SkeletonUnparsable {} "`vyrn\"…\"` skeleton does not parse as Vyrn code";
    ImportNamespaceBuiltin { spec }
        "`{spec}` cannot be imported as a namespace (`import * as`) — its names \
        are builtins; import them by name or use them directly";
    ImportNoExport { spec, name } "{spec} has no export `{name}`";
    GenImportConstArgs { name }
        "generator import `{name}(..)` needs compile-time-constant arguments (v1: \
        string / integer / boolean literals)";
    GenImportNotGen { name }
        "`{name}` is not an imported `gen fn` — a generator import target must be an \
        exported `gen fn` in a module this file imports";
    GenImportArity { name, want, got } "generator `{name}` takes {want} argument(s), got {got}";
    GenModuleReread { module, why } "cannot re-read generator module `{module}`: {why}";
    GenFailed { name, args, trap } "generator `{name}({args})` failed: {trap}";
    NamespaceBoundTwice { ns } "namespace `{ns}` is bound twice in this module";
    NamespaceCollides { ns }
        "namespace `{ns}` collides with a top-level declaration or import \
        of the same name in this module";
    ImportedTwice { local } "`{local}` is imported twice into this module";
    AliasClashes { local }
        "import alias `{local}` clashes with a top-level declaration of \
        the same name in this module";
    ImportedUnderAlias { orig, local }
        "`{orig}` is not in scope — it was imported as `{local}`; use \
        that name (or import `{orig}` too)";
    NamespaceNoMember { ns, target, member }
        "namespace `{ns}` (module `{target}`) has no exported member `{member}` — \
        namespaces reach exported declarations only, one level deep";
    NotNamespace { ns } "`{ns}` is not an in-scope namespace";
    NamespacedVariant { head, enum_name, variant }
        "`{head}.{enum_name}.{variant}` is not a namespaced enum \
        variant (namespaces are one level deep)";
    NamespaceNotValue { name } "namespace `{name}` is not a value";
    NotExported { name, target }
        "`{name}` exists in `{target}` but is not exported — \
        add `export` to its declaration";
    NotDefinedIn { name, target, def_module }
        "`{name}` is not defined in `{target}` (it lives in \
        `{def_module}`)";
    TargetLacks { target, name } "`{target}` does not define `{name}`";
    NotImported { what, name, def_module }
        "{what} `{name}` is defined in `{def_module}` but not \
        imported here — add it to an `import {{ .. }} from` list";
    NotImportedList { what, name, list }
        "{what} `{name}` is defined in `{list}` but not imported here — add \
        it to an `import {{ .. }} from` list";

    // The ownership and typed judgments' rules, which `vyrn-lower` states.
    // A hole there is text the site renders: a type as its body speaks it,
    // or a clause such as a taker or a borrow's kind. A row named for a way
    // out (`CopyForJs`, `SwapRemove`) renders one fix line, which a site
    // adds under another row when its own state picks it.
    //
    // Shapes A to D: the kernel's flow rules, at a use (A), where a scope
    // ends (B), where edges join (C) and at a loop's back edge (D).

    // A use after a declared `consume`, a `drop`, or a linear value's take.
    Consumed { read, what, by, l }
        "`{read}` is {what} here but was already consumed by {by} on line {l}\n  (a \
        `consume` parameter takes ownership; the value can't be used afterward)";
    DroppedAfterConsume { read, by, l }
        "`{read}` is dropped here but was already consumed by {by} on line {l}";
    // A use after any other take, at the take.
    Moved { s, by, here, what }
        "`{s}` was moved here into {by}\nline {here}: ... and `{s}` is {what} again here"
        fix "`{s}.copy()` if both sides need a value";
    // A use after a placed release.
    Released { s, what } "`{s}` is {what} here after it was released";
    // A read of an alias whose place was written, at the write.
    AliasRead { place, s, here, what, src, at }
        "`{place}` is written here while `{s}` still reads out of it\nline {here}: ... and \
        `{s}` is {what} again here"
        fix "`{src}.copy()` on line {at}, so `{s}` is a value of its own";
    // A `consume` argument and a place overlapping it, handed to one call.
    ConsumedAndPassed { s, by, o }
        "`{s}` is consumed by {by}, and `{o}` is passed to the same call, so the callee could \
        read what it frees"
        fix "`{s}.copy()` for the `consume` parameter";
    // An argument that reads a global the callee stores into.
    StateRead { place, s, here, what }
        "`{place}` is written here while `{s}` still reads out of it\nline {here}: ... and \
        `{s}` is {what} again here";
    WholeWithHole { s, path, here, l }
        "`{s}{path}` was taken out of `{s}` here\nline {here}: ... and `{s}` is used as a \
        whole here, with the hole still in it"
        fix "`{s}{path}.copy()` on line {l} if `{s}` is still needed whole"
        fix "write `{s}{path}` back before this line";
    ReadInHole { s, h, here }
        "`{s}{h}` was moved here into `consume`\nline {here}: ... and `{s}{h}` is used again \
        here"
        fix "`{s}{h}.copy()` if both sides need a value";
    StoreUnderHole { s, h, here, path }
        "`{s}{h}` was moved here into `consume`\nline {here}: ... and `{s}{path}` is written \
        here, under the hole";
    // A payload binder handed on out of a type that declares `release`.
    SealedPayload { b, ty, m }
        "`{b}` may not be handed to a `consume` parameter: `{ty}` declares `release`, which \
        reads it; consume `{m}` or copy `{b}`";
    // A written `drop` of a name with a hole.
    DropWithHole { s, h, l }
        "`{s}` may not be dropped — `{s}{h}` was taken out of it on line {l}, and `drop` \
        releases the whole binding"
        fix "write `{s}{h}` back before the `drop`, so the binding is whole again"
        fix "delete the `drop` — the parts still here are released when the block exits";
    ReleasedWithHole { info, h }
        "{info} is released whole although a `consume` took `{h}` out of it";
    ReleasedAround { info, h } "{info} is released around `{h}` on a path that did not take it";
    Overwritten { info }
        "{info} is overwritten while still held — the old value is never released";
    ReleasedBeforeStore { info }
        "{info} is released before a store although it holds nothing";
    HeldAtExit { info, exit } "{info} is still held at {exit} — no release is placed for it";
    HeldAtArmEnd { info }
        "{info} is still held where its arm ends — no release is placed for it";
    JoinMoved { s, by }
        "`{s}` was moved here into {by} on one path and not on the other, and nothing \
        releases it where the paths join";
    JoinReleased { s }
        "`{s}` is released on one path and still held on another where the paths join";
    JoinHole { info } "{info} has a `consume` hole on one edge of a join and not on another";
    LoopMoved { s, by }
        "`{s}` is consumed by {by} inside a loop, so it would be used again on the next \
        iteration";
    LoopReleased { s }
        "`{s}` is released inside a loop, so it would be used again on the next iteration";
    LoopBound { info } "{info} is bound inside a loop that would use it again on the next turn";
    LoopHole { s, h }
        "`{s}{h}` is consumed by `consume` inside a loop, so it would be used again on the \
        next iteration"
        fix "`{s}{h}.copy()` if both sides need a value";
    LoopHoleAt { info }
        "{info} has a `consume` hole at a loop's back edge it did not have at entry";

    // A linear binding a path leaves held, or disposes of inside a loop or on
    // one branch only, said once at the binding. `owed` in `vyrn-lower`
    // recognizes these two rows.
    NeverDisposed { s, a, ty } "`{s}` is {a} `{ty}` and is never disposed";
    // A linear binding used after its disposal, at the binding.
    DisposedTwice { s, a, ty } "`{s}` is {a} `{ty}` and is disposed more than once";
    // The note under either must-use row, by the row that obliges the type. A
    // stream's release is pushed by its own lowering, so `drop` on one
    // reclaims nothing; a declared type has no `close` and is not iterable
    // unless it says so.
    OwedStream { s }
        "a stream must be consumed with `for … in`, forwarded by returning it, or released \
        with `close({s})` — on every path";
    OwedDeclared { ty, s }
        "`{ty}` declares `impl MustUse`, so a value of it must be handed on by name — passed \
        to a call, forwarded by returning it, or released with `drop {s}` — on every path";
    // A container: the reader wrote `Array<Txn>` and the row is `Txn`'s.
    OwedHeld { by, ty, s }
        "`{by}` declares `impl MustUse` and a `{ty}` holds one, so the container must be \
        handed on by name — passed to a call, forwarded by returning it, or released with \
        `drop {s}`, which releases each element — on every path";

    // Shape E, the flow-free rules: the kernel's at its use sites.

    // A `return` of a closure's captured binding.
    ReturnedCapture { s }
        "`{s}` may not be returned from a closure — it is a captured binding, and the \
        closure's result is its caller's";
    // A `return` of a borrow from an `export extern fn`: the JS caller releases
    // what it is handed.
    ReturnedToJs { s, what }
        "`{s}` may not be returned from an exported function — it is {what}, and the JS \
        caller releases what it is handed";
    ReturnedBorrow { s, what } "`{s}` may not be returned — it is {what}, and a return is owned";
    // A take of a borrow; `may_not` names the taker.
    TakenBorrow { may_not, what } "{may_not} — it is {what}";
    CopyToOwn { s } "`{s}.copy()` if the value should own it";
    CopyForCaller { s } "`{s}.copy()` if the caller needs its own value";
    CopyForJs { s } "`{s}.copy()` — an `export extern fn` owns its result";
    CopyFromJs { s }
        "`{s}.copy()` — an `export extern fn` may not take ownership of a String its JS \
        caller releases";
    CopyForCallee { path } "`{path}.copy()` — the callee owns its copy";
    CopyBoth { path } "`{path}.copy()` if both sides need a value";
    ConsumeParam { of } "declare the parameter `{of}: consume ..` if this function should own it";
    ConsumeNamedFn { of }
        "a named function with `{of}: consume ..`, called directly, if it should own it";
    ForInConsume { path, of } "`for {path} in consume {of}` if the loop should take the elements";
    ConsumePrefix { path, root }
        "`consume {path}` if `{root}` should give it up — the field is dead afterwards";
    // Module state read whole, handed to a `consume` parameter.
    ModuleStatePassed { g, by }
        "module state `{g}` may not be passed to a `consume` parameter via {by} — nothing may \
        take ownership of module state (it lives for the whole module and is never dropped)";
    ModuleStateConsumed { g, by }
        "module state `{g}` may not be consumed by {by} — nothing may take ownership of \
        module state (it lives for the whole module and is never dropped)";
    // Module state, or a projection of it, returned.
    ReturnedModuleState { s }
        "`{s}` may not be returned — it is module state, which nothing may take, and a return \
        is owned"
        fix "`{s}.copy()` — the caller releases what it is handed";
    TakenModuleState { may_not, s } "{may_not} — it is module state, which nothing may take"
        fix "`{s}.copy()` — the callee releases what it is handed";
    // A named binding read out of a place, which a call rebuilds.
    RebuiltBorrow { s, src, here, by }
        "`{s}` is read out of `{src}` here — a place that owns it\nline {here}: ... and {by} \
        takes `{s}`, so `{s}` must be a value of its own"
        fix "`{src}.copy()` if `{s}` should own what {by} rebuilds";
    DroppedBorrow { s, kind } "`{s}` may not be dropped — it is {kind}"
        fix "`consume` the place where `{s}` is bound, so `{s}` takes the value rather than \
            naming it"
        fix "delete the `drop` — the place that owns it releases it";
    EscapingCapture { s, what }
        "`{s}` may not be captured by a closure that outlives this call — it is {what}";
    ReleasedUnowned { info } "{info} is released although the body does not own it";
    StoreReleasesNothing { l } "a store into a place that owns heap releases nothing (line {l})";

    // Shape E: the builder's, at the construct.

    ElementTaken { path } "`{path}` may not be taken — an element is not a place a take reaches";
    SwapRemove { root }
        "`{root}.swapRemove(..)` returns the element and leaves the container one shorter";
    LoopTakesNothing {}
        "`consume` here has nothing to take — the loop already owns a container that is not a \
        binding"
        fix "drop the `consume`: the elements are already owned";
    ConsumeTakesNothing {}
        "`consume` here has nothing to take — the value is already owned, so there is no \
        place to leave a hole in"
        fix "drop the `consume`: the value is already owned";
    ConsumedBorrow { root, what } "`{root}` may not be consumed — it is {what}";
    HandedOutOfLoopArm { a }
        "`{a}` may not be handed out of an arm inside a loop — the result is released on \
        every turn, and `{a}` is bound outside the loop"
        fix "`{a}.copy()` if the arm should hand out a value of its own";

    // Shape E: `typed`'s, over every row.

    StoreRuled { n, name }
        "cannot mutate a field of `{n}` in place (its `where` invariant could be broken \
        mid-update); rebuild it: `{name} = {n} {{ .. }}`";
    GroupRead { name }
        "`{name}` is read whole while a store into its field leaves its `where` rule \
        unchecked"
        fix "read `{name}` before the first store into its fields, or after the last";
    GroupCall { f, name }
        "`{f}` may read the caller's `{name}` while a store into its field leaves its `where` \
        rule unchecked"
        fix "call `{f}` before the first store into the fields of `{name}`, or after the last";
    GroupExit { what, name } "`{what}` leaves `{name}` with its `where` rule unchecked"
        fix "finish the stores into the fields of `{name}` before the `{what}`";
    GroupFalse { name, k, long, short, n }
        "this group of stores into `{name}` ends after line {k} with `{name}.{long}` longer \
        than `{name}.{short}`, which breaks the `where` rule of `{n}`"
        fix "store into `{name}.{short}` before any statement after line {k} that does not \
            store into `{name}`";
    RemoveNotMut { op, name } "cannot `{op}` from `{name}` (declared without `mut`)";
    AssignNotMut { name } "cannot assign to `{name}` (declared without `mut`)";
    FieldNotMut { name } "cannot mutate a field of `{name}` (declared without `mut`)";
    StoreNotMut { name } "cannot store into `{name}` (declared without `mut`)";
    OutsideLoop { what } "`{what}` outside a loop";
    DropModuleState { name }
        "cannot `drop` module state `{name}` — it lives for the whole module and is reclaimed \
        at process exit";
    DropUnbound { name } "`drop` of unbound variable `{name}`";
    DropTypeParam { name, t }
        "cannot `drop` `{name}`: its type `{t}` is a type parameter, so this body cannot know \
        whether the rule below holds for the instance — a plain record would be released \
        here where `drop` on it directly is refused. Release the value where its concrete \
        type is known, or `consume` the heap field and `drop` that";
    DropNotHeap { name, t }
        "`drop` needs a heap value (a String, an Array, a Map, a Ref, or an Option/Result \
        carrying one, or a type declaring `impl Owned`), but `{name}` is {t}";

    // A body the core builder did not build: a defect in the builder, since
    // the checker typed the body.
    CoreGap { what, body } "internal error: the core cannot state {what}, so `{body}` is not judged";
    CoreGapAt { what, detail, body }
        "internal error: the core cannot state {what} `{detail}`, so `{body}` is not judged";

    // The checker's sentences for what it types `Err`, which the builder
    // states from the facts.

    FunctionFallsThrough { name, owes } "function `{name}` must return {owes} on all paths";
    LambdaFallsThrough { owes } "this lambda must return {owes} on all paths";
    ShiftOutOfRange { amt, bits }
        "shift amount {amt} is out of range for a {bits}-bit value (valid range is 0..{bits})";
    InvalidRegex { pat, err } "invalid regex `{pat}`: {err}";
    MatchNeedsPattern {} "the right side of `=~` must be a string-literal pattern";
    ArrayLiteralTooLong { len, limit }
        "this array literal has {len} elements, past the limit of {limit}\n  note: a literal \
        is lowered element by element into one call frame, so its length is a compile-time \
        cost on both backends\n  note: a table this long belongs in a file the program \
        reads, not in the program";
    SmallArrayOverflow { len, n } "this literal has {len} elements but the slot is SmallArray<_, {n}>";
    NegNeedsNumber { t } "unary `-` needs a numeric type, found {t}";
    NotNeedsBool { t } "unary `!` needs Bool, found {t}";
    BitNotNeedsInteger { t } "unary `~` needs an integer type, found {t}";
    NoField { ty, field } "type {ty} has no field `{field}`";
    StringLength {}
        "String has no `length`: use `byteLength` for bytes or `charCount()` for Unicode \
        scalars";
    FieldOnNonRecord { field, other } "cannot access field `{field}` on non-record type {other}";
    TryConstructNotScalar { name } "`{name}?(..)` is only for validated/nominal scalar types";
    TryConstructArity { name, got } "`{name}?` takes 1 argument, got {got}";
    ConstructArity { name, got } "`{name}` construction takes 1 argument, got {got}";
    ConstructFrom { name, base, aty } "`{name}` is built from {base}, but the argument is {aty}";
    NotRecordType { name } "`{name}` is not a record type";
    RecordNoField { name, field } "record `{name}` has no field `{field}`";
    FieldSetTwice { field } "field `{field}` set twice";
    MissingField { field, name } "missing field `{field}` for `{name}`";
    VariantNeedsArgs { name, payload } "variant `{name}` needs {payload} argument(s)";
    CallArity { shown, want, got } "`{shown}` expects {want} argument(s), got {got}";
    NoTypeParams { shown }
        "`{shown}` declares no type parameters, so it takes no type arguments";
    ReceiverType { shown, pty, aty } "the receiver of `{shown}` expects {pty}, found {aty}";
    ForgetsHeap { shown, t }
        "`{shown}` forgets or overwrites elements without releasing them, and `{t}` owns heap \
        \u{2014} move the elements one at a time instead";
    NotCodable { shown, off } "`{shown}` cannot decode into `{off}` (not a codable type)";
    LambdaArgArity { got, callee, n, want }
        "this lambda takes {got} parameter(s), but `{callee}` argument {n} expects {want}";
    LambdaReturns { t, callee, r }
        "this lambda returns {t}, but `{callee}` expects it to return {r}";
    // `subject` and `owner` name the value: its binding, or "this".
    ValueArity { subject, got, callee, n, want }
        "{subject} is a {got}-argument function value, but `{callee}` argument {n} expects \
        {want}";
    ValueParam { owner, a, callee, b }
        "{owner} expects a {a} argument, but `{callee}` will pass it {b}";
    GenericFnArg { vn } "`{vn}` is generic and cannot be passed as a function value in v1";
    FnArity { vn, got, callee, n, want }
        "`{vn}` takes {got} argument(s), but `{callee}` argument {n} expects a {want}-argument \
        function";
    NotFnArg { callee, n, aty }
        "`{callee}` argument {n} must be a lambda `|..| ..`, a function name, or an expression \
        of `fn` type; found {aty}";
    LambdaReturnsSlot { t, exp, r }
        "this lambda returns {t}, but the expected function type `{exp}` returns {r}";
    FnAritySlot { name, got, exp, want }
        "`{name}` takes {got} argument(s), but the expected function type `{exp}` takes {want}";
    FnReturnsSlot { name, t, exp, r }
        "`{name}` returns {t}, but the expected function type `{exp}` returns {r}";
    BindUnit { name } "cannot bind `{name}` to a Unit value";
    AssignMismatch { name, to, vty } "`{name}` is {to} but assigned {vty}";
    ReturnMismatch { ret, vty } "return type mismatch: expected {ret}, found {vty}";
    ForNeedsIterable { t }
        "`for` needs an Array, a String, or a type that declares `impl Iterate` (a `size` \
        method and an `nth` projection, `fn nth(read self, ..) -> read T`), found {t}";
    NotRecordNoField { name, field } "`{name}` is not a record, so it has no field `{field}`";
    FieldValidated { field, fty }
        "field `{field}` is {fty} (validated); assign an already-constructed `{fty}` value, \
        e.g. `{fty}(..)`";
    FieldMismatch { field, fty, vty } "field `{field}` is {fty} but assigned {vty}";
    MapStoreKey { name, key, k } "`{name}` is keyed by {key}, but the key here is {k}";
    MapStoreValue { name, val, v }
        "`{name}` holds values of type {val} but the stored value is {v}";
    IndexStoreNoContainer { name, other }
        "`{name}[i] = ..` needs an Array, a Map, or a type whose impl declares the `atSet` \
        projection (`fn atSet(modify self, ..) -> modify T`), found {other}";
    ArrayIndexType { i } "array index must be an Int64, found {i}";
    IndexStoreKey { name, key, i } "`{name}[..] = ..` is keyed by {key}, found {i}";
    ElementMismatch { name, elem, v } "`{name}` holds {elem} but the stored value is {v}";
    AssignUnknown { name } "assignment to unknown variable `{name}`";
    FieldAssignUnknown { name } "assignment to field of unknown variable `{name}`";
    IndexAssignUnknown { name } "index-assignment to unknown variable `{name}`";
    ConditionNotBool { word, t } "`{word}` condition must be Bool, found {t}";
    UnknownVariable { name } "unknown variable `{name}`";
    ShrinkFixedArray { op }
        "`{op}` is not available on a fixed-size array (it cannot shrink); use a growable \
        `Array<T>`";
    ShrinkNeedsArray { op, t } "`{op}` needs an `Array<T>`, found {t}";
    DuplicateArm { v } "duplicate `{v}` arm";
    MissingVariant { v } "`match` is missing variant `{v}`";
}
