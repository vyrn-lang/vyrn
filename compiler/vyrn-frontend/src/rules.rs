//! The rules the lexer, parser, loader and checker state, one row each: the
//! rule, the holes its sentence names, the sentence, and the fixes `vyrn fix`
//! reads under it. A [`Diagnostic`](crate::diagnostics::Diagnostic) built from
//! a [`Rule`] carries it, and its message is [`Rule::render`].

use crate::diagnostics::menu;
use crate::types::{FALLIBLE, SHOW, SHOW_SHOW};

const HINT: &str = "generators run at compile time — they may not use `extern`, \
                    module state, `print`, `writeFile`, `readLine`, `args`, `readFileBytes`, \
                    the clock, entropy, or logging sinks";

macro_rules! rules {
    ($($rule:ident { $($hole:ident),* } $text:literal $(fix $fix:literal)*;)*) => {
        /// A rule the compiler states, with the rendered text of each hole.
        #[derive(Debug, Clone)]
        pub enum Rule {
            $($rule { $($hole: String),* },)*
        }

        impl Rule {
            /// Renders the sentence, then one fix line per way out.
            pub fn render(&self) -> String {
                match self {
                    $(Rule::$rule { $($hole),* } => {
                        menu(format!($text), Vec::<String>::from([$(format!($fix)),*]))
                    })*
                }
            }
        }
    };
}

/// Builds the error that states `Rule::$rule` for `$stage` at `($line, $col)`.
/// A hole is filled by the variable of its name or by `hole = expr`, either
/// through `Display`.
macro_rules! refuse {
    (@hole $h:ident) => {
        $h.to_string()
    };
    (@hole $h:ident $e:expr) => {
        $e.to_string()
    };
    ($stage:expr, $line:expr, $col:expr, $rule:ident $(, $h:ident $(= $e:expr)?)* $(,)?) => {
        $crate::diagnostics::Diagnostic::refusal(
            $line,
            $col,
            $stage,
            $crate::rules::Rule::$rule { $($h: $crate::rules::refuse!(@hole $h $($e)?)),* },
        )
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
    GlobalInitMismatch { name, declared, vty }
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
    ValueType { t, hint } "`value` boxes an Int64, Bool, or String, found {t}{hint}";
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
    NeedsShow { shown, found, hint }
        "`{shown}` needs a number, Bool, or String, found {found}{hint}";
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
}
