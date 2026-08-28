# Census — attributes

The question: Rust has attributes (`#[test]`, `#[derive(..)]`, `#[cfg(..)]`). Does Vyrn have anything like them, and what should it have?

This file collects what Vyrn decorates declarations with today, what six other languages do, and how Vyrn's `gen fn` mechanism changes the design space. It ends with options, not a choice.

---

## What an attribute is, in one sentence

An attribute is metadatum attached to a declaration or expression, read by a tool or compiler phase that is not the ordinary type checker, to decide something the source alone does not say.

The design axes that separate every real system:

- **Data or code.** Is the attribute a passive value the consumer reads, or a program the compiler runs?
- **When it is read.** Lex time, parse time, type-check time, link time, run time, or never (inert, for documentation only).
- **Who can define one.** The compiler only, or any user.
- **What it can change.** Only annotate (inert, read by tools), or transform the code (active, the declaration is rewritten or replaced).
- **How it is type-checked.** Not at all, by a fixed schema, by the same checker as the rest of the language, or by a separate processor that can emit diagnostics.

---

## What other languages have

### Rust — `#[..]`, three forms, some user-defined

Rust has one syntax, `#![..]` (inner) and `#[..]` (outer), and the attribute is a path plus an optional input: a delimited token tree or `= expression` (https://doc.rust-lang.org/reference/attributes.html).

The three forms are not three syntaxes but three kinds of consumer:

1. **Built-in attributes** — `#[cfg(..)]`, `#[allow(..)]`, `#[inline]`, `#[test]`, `#[no_mangle]`. The compiler knows each by name. Some are active (`cfg` removes the item, `test` makes it a test); most are inert hints to codegen or lint.
2. **Derive macros** — `#[derive(Serialize)]` calls a user-written proc-macro that reads the struct's fields and emits an `impl`. The input is the AST; the output is new source.
3. **Attribute proc-macros** — `#[my_attr(..)] fn foo() { .. }` hands the whole function's `TokenStream` to a user proc-macro and replaces it with whatever the macro returns.

|axis|Rust|
|---|---|
|data or code|both: built-ins are data; proc-macros are arbitrary native code|
|when read|parse/expand (active), type-check (derive output is checked), codegen (inert hints)|
|user-defined|yes, via proc-macros (derive and attribute), which are separate crates compiled as native code|
|what it changes|active attributes replace the form they sit on; derive adds new items alongside|
|type-checked|built-ins by the compiler's own rules; proc-macro output is re-parsed and type-checked like any source|

The cost: proc-macros run arbitrary native code with ambient authority, can read files, make network calls, and are not sandboxed (https://doc.rust-lang.org/reference/procedural-macros.html). The hygiene problem is real and Rust's answer is a macro-by-macro discipline, not a language guarantee.

### Java — annotations, retention policies, annotation processors

Java annotations are `@Name(..)` on declarations, with three retention policies: `SOURCE` (discarded by the compiler), `CLASS` (written to bytecode, not readable at run time), `RUNTIME` (readable via reflection) (https://docs.oracle.com/javase/specs/jls/se22/html/jls-9.html#jls-9.6.4).

|axis|Java|
|---|---|
|data or code|data only — an annotation is a map of typed constants|
|when read|compile time (annotation processors), class-load time (`CLASS`), run time (`RUNTIME` reflection)|
|user-defined|yes — any `@interface` declaration is a new annotation type|
|what it changes|an annotation processor can generate new source files, but cannot modify the annotated declaration itself (the Java compiler forbids in-place transformation)|
|type-checked|the annotation's elements are type-checked against the `@interface` definition; targets are checked against `@Target`|

The restraint: annotations cannot rewrite the code they sit on. An annotation processor generates *new* files; the annotated class is untouched. This is the opposite of Rust's attribute proc-macros.

### C# — `[Attribute]`, reflection, `AttributeUsage`

C# attributes are `[AttributeName(..)]`, square brackets, applied to assemblies, types, methods, fields, properties. Each attribute is a class inheriting `System.Attribute`, marked with `[AttributeUsage(..)]` to say where it may appear and whether it may repeat (https://learn.microsoft.com/en-us/dotnet/csharp/language-reference/language-specification/attributes).

|axis|C#|
|---|---|
|data or code|data — an attribute is a constructor call whose arguments are constants|
|when read|run time, via reflection (`GetCustomAttribute`); some are read by the compiler (`[Conditional]`, `[Obsolete]`)|
|user-defined|yes — any class extending `Attribute`|
|what it changes|inert for the program; active only for the compiler's own (`[Conditional]` removes calls, `[Obsolete]` emits a warning)|
|type-checked|the constructor arguments are type-checked; `AttributeUsage` is enforced at the attribute's own declaration|

### Python — decorators, code, runs at definition time

Python decorators are `@decorator` above a `def` or `class`. A decorator is a callable that takes the function or class and returns a replacement (https://docs.python.org/3/reference/compound_stmts.html#function).

|axis|Python|
|---|---|
|data or code|code — a decorator is an ordinary function|
|when read|definition time, immediately, in source order|
|user-defined|yes — any callable|
|what it changes|anything — the return value replaces the definition; `@property` turns a method into a descriptor, `@dataclass` synthesizes `__init__`/`__repr__`|
|type-checked|not by the language; a type checker like mypy understands a fixed set (`@property`, `@staticmethod`) and `@dataclass` by recognizing the name|

### Go — struct tags and build tags, two unrelated things

Go has two mechanisms that share the word "tag":

- **Struct tags** — backtick string literals on fields: `` `json:"name,omitempty" db:"name"` ``. Pure data, read by reflection at run time (`reflect.StructTag`). Not type-checked; a typo is silent.
- **Build tags** — `//go:build linux` comments before the package clause, read by the `go` tool to decide which files compile (https://pkg.go.dev/cmd/go#hdr-Build_constraints).

Neither is user-extensible and neither runs code. They are the minimal end of the spectrum: a string the framework reads, and a comment the build tool reads.

### Zig — `@` builtins and `comptime`, no attributes

Zig has no attribute syntax. It has `@`-prefixed builtins (`@intCast`, `@import`, `@TypeOf`) that are compiler-known functions, and `comptime` — a keyword that forces an expression or declaration to be evaluated at compile time (https://ziglang.org/documentation/master/#comptime).

|axis|Zig|
|---|---|
|data or code|code — `comptime` runs real Zig at compile time|
|when read|compile time, by the compiler's own evaluator|
|user-defined|no new `@` builtins; `comptime` code is user code, so the *body* is user-defined even though the *marker* is fixed|
|what it changes|`comptime` does not transform code; it evaluates it. `@` builtins are functions, not annotations|
|type-checked|by the same type checker as run-time code|

Zig's answer is that compile-time execution is the attribute: if you need to decide something at build time, write `comptime` code that decides it. There is no separate metaprogramming layer.

### Swift — property wrappers and macros

Swift has two mechanisms:

- **Property wrappers** — `@State`, `@Published`, `@UserDefault`. A wrapper is a generic struct with a `wrappedValue`; `@Wrapper` on a stored property synthesizes a backing store and accessors (https://docs.swift.org/swift-book/documentation/the-swift-programming-language/properties/#Property-Wrappers).
- **Macros** (Swift 5.9) — `#expressionMacro`, `#externalMacro`. A macro is a plugin (a separate executable or library) that receives AST and emits AST, run by the compiler at expansion time (https://github.com/swiftlang/swift-evolution/blob/main/proposals/0382-expression-macros.md).

|axis|Swift|
|---|---|
|data or code|property wrappers are code (a struct with a protocol); macros are code (a plugin)|
|when read|compile time, by the compiler's expansion pass|
|user-defined|yes, for both|
|what it changes|property wrappers rewrite a property's storage and access; macros rewrite the expression or declaration|
|type-checked|the expanded code is type-checked; the macro plugin itself is type-checked when built|

---

## The axis table, side by side

|language|data or code|when read|user-defined|changes code|type-checked how|
|---|---|---|---|---|---|
|Rust|both|expand/codegen|proc-macros (native)|active replaces|re-parsed and checked|
|Java|data|compile/runtime|`@interface`|generates new files, not edits|against the `@interface`|
|C#|data|runtime/compiler|`Attribute` subclass|compiler's own only|constructor args|
|Python|code|definition time|any callable|replaces definition|not by the language|
|Go|data|runtime/build tool|no|no|not at all (struct tags)|
|Zig|code|compile time|`comptime` body, not marker|evaluates, not transforms|same checker|
|Swift|code|compile time|yes|rewrites|expanded code is checked|

The spectrum runs from "passive data a framework reads" (Go, C#, Java) through "code the compiler runs at build time" (Zig, Swift macros, Python) to "arbitrary native code with ambient authority" (Rust proc-macros).

---

## The key axis for Vyrn

Vyrn already has a compile-time code mechanism: `gen fn` (RFC-0021). A `gen fn` is an ordinary function the loader may run at compile time to synthesize a module. It is interpreted (or compiled to wasm and run, per RFC-0076), sandboxed, deterministic, and content-address cached.

The sandbox rules are in RFC-0021 §"The sandbox" (`rfcs/RFC-0021-generator-imports.md:54`) and enforced by `check_comptime_purity` in `compiler/vyrn-frontend/src/checker.rs:9502`. A `gen fn` and its transitive callees may not use `extern`, `spawn`, module state, or any builtin in `COMPTIME_FORBIDDEN` (`compiler/vyrn-frontend/src/checker.rs:9478`). Permitted, mediated: `readFile`, `listDir`, `moduleInterface` — all routed through the loader's resolver, scoped to the call's constant path arguments, and recorded as cache inputs (`rfcs/RFC-0021-generator-imports.md:60-77`).

This is the thing that changes Vyrn's design space. In Rust, an attribute proc-macro is the *only* way to run user code at compile time, so attributes and metaprogramming are the same problem. In Vyrn, compile-time code already exists as a first-class, sandboxed, cached mechanism — independent of any attribute syntax. The question is whether attributes add anything that `gen fn` + `import { .. } from genFn(args)` does not already give.

The honest observation: a Vyrn `gen fn` invoked as `import { rpcHandle } from rpcServer("./api")` (`std/rpc.vyrn:393`) already does what a Rust `#[derive(Rpc)]` does — it reads the contract, emits a module, and the import binds the result. The generator is the attribute. What it lacks is a *per-declaration* spelling: `gen fn` synthesizes whole modules, and RFC-0021 §"Out of scope" (`rfcs/RFC-0021-generator-imports.md:175`) states this restraint as a feature, not a gap: "generators synthesize *modules*, nothing smaller — that restraint is the feature."

So an attribute that runs a `gen fn` is not a new mechanism. It is a new *attachment point*: the same sandbox, the same cache, the same purity check, but triggered from a decoration on a single declaration rather than from an import line. What would have to change is the loader's entry path: today `run_generator` (`compiler/vyrn-frontend/src/loader.rs:1526`) is reached only through `ImportSource::Generator` in an import. An attribute form would reach the same function from a declaration's decoration, and the loader would have to splice the generated module's exports back as the declaration's implementation. The sandbox rules need no change — they already govern every `gen fn` unconditionally (`compiler/vyrn-frontend/src/checker.rs:9496`).

---

## What Vyrn has today

Vyrn has no attribute syntax. There is no `#[..]`, no `@..`, no bracketed decorator. What Vyrn has is a set of declaration modifiers, each parsed by its own special case in the parser, each stored as a separate field on the AST node. They share no grammar.

The full inventory of constructs that decorate a declaration:

1. **`export`** — a prefix keyword on `fn`, `type`, `protocol`, `contract`, `extern fn`, `gen fn`, `mut fn`. Stored as `exported: bool` on `Function` (`compiler/vyrn-frontend/src/ast.rs:588`), `TypeDecl` (`compiler/vyrn-frontend/src/ast.rs:250`). The parser handles it in one place with lookahead for each allowed follower (`compiler/vyrn-frontend/src/parser.rs:1273`).

2. **`extern`** — a contextual starter before `fn`. Stored as `is_extern: bool` (`compiler/vyrn-frontend/src/ast.rs:614`). Recognized only when `fn` follows, so a variable named `extern` is unharmed (`compiler/vyrn-frontend/src/parser.rs:1437`).

3. **`export extern`** — the combination. Stored as `is_export_extern: bool` (`compiler/vyrn-frontend/src/ast.rs:623`). This is the construct the assignment names: the `wasm-export-name` attribute. It is not a surface-language attribute. It is an LLVM IR string attribute the codegen emits for the `define` (`compiler/vyrn-codegen/src/lib.rs:4436`): `format!(" \"wasm-export-name\"=\"{}\"", f.name)`. No user writes it. No parser arm reads it. It is a backend emission, not a language feature.

4. **`gen`** — a contextual starter before `fn`. Stored as `is_gen: bool` (`compiler/vyrn-frontend/src/ast.rs:624`). Recognized only when `fn` follows (`compiler/vyrn-frontend/src/parser.rs:1349`). This is the generator marker.

5. **`mut`** — before `fn` (`mut fn`). Stored as `is_mut: bool` (`compiler/vyrn-frontend/src/ast.rs:634`). A declaration, never an inference — nothing checks that a `mut fn` writes anything (`compiler/vyrn-frontend/src/ast.rs:635`). Its value is that the fact is stated in a real symbol (`compiler/vyrn-frontend/src/ast.rs:637`).

6. **`test`** — a contextual starter before a string literal. A separate `TestDecl` AST node (`compiler/vyrn-frontend/src/ast.rs:98`), not a modified function. Recognized only when a string follows (`compiler/vyrn-frontend/src/parser.rs:1457`).

7. **`bench`** — a contextual starter before a string literal. A separate `BenchDecl` AST node (`compiler/vyrn-frontend/src/ast.rs:79`). Recognized only when a string follows (`compiler/vyrn-frontend/src/parser.rs:1475`).

8. **`contract`** — a contextual starter. A separate `ContractDecl` AST node (`compiler/vyrn-frontend/src/ast.rs:337`). Recognized by a lookahead predicate `at_contract_decl` (`compiler/vyrn-frontend/src/parser.rs:1618`). Kept contextual because `std/rpc`, `std/connect`, `std/openapi`, and `std/graphql` all take a parameter named `contract` (`compiler/vyrn-frontend/src/parser.rs:77`).

9. **`logging`** — a top-level config block, not a decorator on a declaration (`compiler/vyrn-frontend/src/parser.rs:1494`). It sets a module-wide threshold and sink. Listed for completeness.

10. **`///` doc comments** — markdown attached by the parser. Stored as `doc: Option<String>` on `Function` (`compiler/vyrn-frontend/src/ast.rs:592`), `TypeDecl` (`compiler/vyrn-frontend/src/ast.rs:255`), and every other declaration that carries one. The lexer produces `TrivKind::Doc` trivia (`compiler/vyrn-frontend/src/lexer.rs:222`); the parser folds consecutive lines and attaches them. This is the closest thing to an inert attribute: data read by `vyrn doc`, the LSP hover, and `schemaOf(T).doc`.

11. **`where` refinement** — a predicate on a type declaration: `type Age = Int64 where value >= 18`. Stored as `predicate: Option<Expr>` on `TypeDecl` (`compiler/vyrn-frontend/src/ast.rs:261`). Also inline field refinements (`compiler/vyrn-frontend/src/parser.rs:2555`). This decorates a type with a validation rule the checker enforces at every construction site. It is active, but it is not a general attribute — it is one fixed construct.

12. **Parameter capabilities** — `read`, `modify`, `consume`, `share` on parameters. The `Capability` enum (`compiler/vyrn-frontend/src/ast.rs:662`). These decorate a parameter and change the move checker's rules. Fixed vocabulary, not extensible.

13. **Generic bounds** — `fn id<T: Ord>`. Stored as `type_bounds` on `Function` (`compiler/vyrn-frontend/src/ast.rs:596`). A fixed set: `Eq`, `Ord`, `Num`.

What nearly exists: `where` refinements and `mut fn` are the two constructs closest to attributes. Both are fixed-name, fixed-semantics decorators with no user extension. Both are checked by the existing checker. Both are inert to the codegen except through the checker's verdict. If Vyrn generalized either, it would have the skeleton of an attribute system. It has not.

The answer to "do they share a grammar": no. Each is a separate `match` arm in `program_accum` (`compiler/vyrn-frontend/src/parser.rs:1248`) or in `function`. Each is a separate boolean on the AST. There is no list, no token-tree input, no general `attribute` parser function. Adding a new modifier means a new parser arm, a new AST field, and a new consumer in the checker or codegen.

---

## The options

RECOMMENDATION, NOT A DECISION

### Option A — nothing: `gen fn` is the attribute, and it is enough

Vyrn already answers the "run user code at compile time" question with `gen fn`. The import site is the attachment point: `import { rpcHandle } from rpcServer("./api")` is a per-call trigger. Inert metadata (docs, the `mut` marker, capabilities) are already first-class fields. Nothing forces a general attribute system, and every language that added one pays for it: Rust's proc-macros are unsandboxed native code; Java's annotations cannot edit code; Python's decorators are unchecked at the type level.

|design|one-sentence description|what it costs in the parser|what it costs in the checker|what it costs in lowering|what breaks in existing code|who else does it|
|---|---|---|---|---|---|---|
|A: nothing|no attribute syntax; `gen fn` and `import { .. } from genFn(args)` cover compile-time code; `///`, `mut`, `where`, capabilities cover inert metadata|zero|zero|zero|nothing|Zig (comptime, no attributes)|

The cost is ergonomic, not technical. A reader who wants `#[derive(Rpc)]` on a struct instead of an import line pays it in a second file. The RPC, i18n, OpenAPI, GraphQL, and UI generators all work today through imports (`std/rpc.vyrn:393`, `std/i18n.vyrn`, `std/openapi.vyrn:236`, `std/graphql.vyrn:828`, `std/ui.vyrn:68`). None asked for an attribute form.

### Option B — inert-only attributes: a fixed `@name(value)` grammar, no code

A general syntax for inert metadata, checked against a fixed table of known names. The compiler knows `@deprecated("use bar")`, `@exportName("vyrnAdd")`, `@since("1.2")`. A name not in the table is a parse error. No proc-macro, no transformation, no user definition. The attribute is data the checker validates and the LSP/doc tool reads.

|design|one-sentence description|what it costs in the parser|what it costs in the checker|what it costs in lowering|what breaks in existing code|who else does it|
|---|---|---|---|---|---|---|
|B: inert-only|`@name(value)` on declarations; fixed known names; no user definition, no code runs|one new arm in `program_accum` and per-decl parsers; a token-tree reader for the `(..)` input|a table of known names and their value types; validate each|codegen reads the few that matter (`@exportName` replaces the `is_export_extern` boolean's string emission); most are dropped|the `wasm-export-name` string becomes a user-facing `@exportName` if desired, or stays internal; no source change forced|Go struct tags, C# built-in attributes, Java `SOURCE`-retention annotations|

The cost is a second parsing path for decoration that is not a contextual keyword. The gain is a place to put `@deprecated` and `@since` without a new reserved word each time. The risk is that inert attributes accumulate names the compiler must know, and each is a maintenance commitment. This does not touch `gen fn` at all.

### Option C — attribute as `gen fn` trigger: `@genFn(args)` runs a generator on the declaration

The attribute is sugar for a generator import. `#[rpcServer("./api")]` on a module, or `@cli on type Command { .. }`, desugars to `import { .. } from genFn(args)` and binds the generated exports alongside the declaration. The generator is an ordinary `gen fn`, already sandboxed and cached. The attribute adds only the attachment point and the splice.

|design|one-sentence description|what it costs in the parser|what it costs in the checker|what it costs in lowering|what breaks in existing code|who else does it|
|---|---|---|---|---|---|---|
|C: gen-fn trigger|`@genFn(args)` on a declaration desugars to `import { .. } from genFn(args)`; the generator is an ordinary `gen fn`, sandboxed and cached as today|a new arm that reads `@ident(args)` before a declaration; the loader's `run_generator` is reached from a declaration, not only an import|nothing new — `check_comptime_purity` already governs every `gen fn` (`compiler/vyrn-frontend/src/checker.rs:9502`); the generated module is checked like any other|nothing — the generated module lowers like any synthesized module|nothing, if it is pure sugar over the existing import path; the import form stays valid|Rust derive macros (but sandboxed and deterministic, unlike Rust's)|

What would have to change: the loader's entry path. Today `run_generator` (`compiler/vyrn-frontend/src/loader.rs:1526`) takes a generator name and args from an `ImportSource::Generator`. An attribute form would call the same function from a declaration's decoration, then merge the generated module's exports into the importing module's namespace — the same merge the import path already does. The sandbox rules (`rfcs/RFC-0021-generator-imports.md:54`) need no change; they are a property of `gen fn`, not of the trigger.

The open question this design must answer: what does the generator *see*? An import-triggered generator gets its args and reads files. A declaration-triggered generator would also need the declaration's AST — the struct fields, the function signature, the contract members. That is `moduleInterface` generalized to a single declaration, or a new reflection primitive. RFC-0021 already gives generators `moduleInterface(path)` (`rfcs/RFC-0021-generator-imports.md:65`); a per-declaration form would reflect the *current* module's declaration, which is a different thing — the module is not yet loaded when the generator runs. This is the real cost, and it is in the loader, not the parser.

### Option D — full attribute grammar with user-defined active attributes

A general `@[name(args)]` syntax where `name` resolves to a `gen fn` in scope. The attribute runs the `gen fn` with the declaration's reflection as an argument and replaces or augments the declaration with the generated output. This is C plus per-declaration reflection plus replacement semantics.

|design|one-sentence description|what it costs in the parser|what it costs in the checker|what it costs in lowering|what breaks in existing code|who else does it|
|---|---|---|---|---|---|---|
|D: full active|`@[genFn(args)]` resolves `genFn` in scope, runs it with the declaration's reflection, and splices the output|the `@` arm plus name resolution against the module's imports before the declaration is checked|the generated splice is checked; the `gen fn` is purity-checked as today; a new "what may a splice replace" rule|the splice lowers like the declaration it replaced|risk: if a splice replaces a declaration, existing code that names it may see a different shape; needs a stable-name contract|Swift macros, Rust attribute proc-macros (but sandboxed)|

This is the most expressive and the most expensive. It requires per-declaration reflection (the open question from C), a splice semantics (does the attribute replace, wrap, or add beside?), and a hygiene story for names the generated code introduces. RFC-0021 §"Hygiene stance" (`rfcs/RFC-0021-generator-imports.md:148`) says v1 has no macro hygiene and generators own their namespace choices. A per-declaration splice sharpens that problem: a generator that emits a name colliding with a sibling declaration is a load error today (flat namespace, `compiler/vyrn-frontend/src/loader.rs:3416`); a replacement splice would need to say whether the original name survives.

### Option E — contextual keyword per feature, continued

Do not add a general grammar. Keep adding contextual keywords for each new decorator, as `mut`, `gen`, `extern`, `test`, `bench`, and `contract` were added. Each is one parser arm and one AST boolean. This is what Vyrn does today and has done for 107 RFCs.

|design|one-sentence description|what it costs in the parser|what it costs in the checker|what it costs in lowering|what breaks in existing code|who else does it|
|---|---|---|---|---|---|---|
|E: keyword per feature|each new decorator is a new contextual keyword and a new boolean on the AST, parsed by its own arm|one arm per decorator, growing|one consumer per decorator|one consumer per decorator, where codegen cares|a contextual keyword can collide with an identifier (the reason `contract` stayed contextual, `compiler/vyrn-frontend/src/parser.rs:77`); each new one is a collision risk to audit|early Rust (before proc-macros), Go (no attributes, build tags are comments)|

The cost compounds. Each keyword is a collision audit, a documentation entry, and a parser arm that cannot share code with the others. The `export` prefix already has a six-way lookahead (`compiler/vyrn-frontend/src/parser.rs:1277-1289`). A seventh follower is cheap; a twentieth is not. This option closes the door on user-defined decorators without saying so.

## Decision (2026-08-28)

**Option A — nothing: `gen fn` is the attribute system, and the thirteen contextual modifiers stay what they are.** The survey's own axis says the split: attributes earn their grammar when third parties consume metadata the compiler ignores, and Vyrn has no third party — every current modifier has exactly one consumer, the compiler, which is what a keyword is for. Option B (inert-only) is acknowledged as the fallback shape the day a real external consumer appears — it breaks nothing and is the smallest door — and option E's compounding cost (a collision audit per keyword, a seventh follower after `export`'s six-way lookahead) is accepted as real but not yet binding. Reopen at the first tool that needs to read declaration metadata the compiler does not.
