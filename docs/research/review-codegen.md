# Review: the code generation backends, five lenses

An external review of Vyrn's two codegen backends at `52e462f`: the textual
LLVM-IR backend (`compiler/vyrn-codegen/src/lib.rs`, the reference native
path), the direct wasm backend (`compiler/vyrn-codegen/src/direct.rs`,
RFC-0077), and the layer they share (`layout.rs`, `toolchain.rs`, `wasm.rs`).
Five reviewers, each with its own values. Read-only; nothing was fixed.

Every finding carries evidence. A code finding cites `file:line`. A
behavioural finding carries a program that was run, with output recorded from
all three engines where they build. Findings are ranked **CONFIRMED**
(reproduced or measured) above **PLAUSIBLE** (argued from reading, not run).
Where an RFC records a decision with its argument, the entry says "design
critique", not "defect".

A separate review already covered the C shim and the prelude, and another
covered a first wave of backend findings; neither set is re-derived here.

---

## Top findings by severity

| # | Severity | Lens | Finding | Ref |
|---|---|---|---|---|
| 1 | **Critical** | Agda | **The textual backend's symbol mangle is not injective, and the driver dedups instantiations by symbol.** A generic instantiated at `Option<Int64>` and at a user type named `OptInt64` emits ONE body under one symbol; `vyrn check` says ok, interp and wasm print `9`, native prints `1948444778241`. The direct backend's own comment (`direct.rs:716`) documents this exact hole — in the sibling it did not fix. | G4.1 |

(Interim commit: this table grows as the remaining findings land. G4.1 is
banked first because it is a silent wrong answer from the reference backend.)

---

## Lens 4 — Agda implementer: soundness

### G4.1 CONFIRMED — Critical. Mangle collision: two instantiations, one symbol, native answers wrong

**What breaks.** `mangle_name` (`compiler/vyrn-codegen/src/lib.rs:12442`)
builds a generic instantiation's symbol as `vyrn_{name}__{mangled type args}`,
and `mangle_ty` (`lib.rs:12460`) is not injective:

- `Type::Option(inner)` mangles as `Opt{inner}` (`lib.rs:12470`) — so
  `Option<Int64>` is `OptInt64`, the same string a **user type named
  `OptInt64`** produces through `Type::Named(n) => sanitize(n)`
  (`lib.rs:12469`).
- The same prefix ambiguity holds for `Arr` (`Array<Int64>` vs a type named
  `ArrInt64`), `Res`, `Strm`, `Map`, `Task`, and for `Type::App`'s
  separator-free concatenation (`P<Int8, Int64>` vs `P<Int8Int64>`,
  `lib.rs:12474-12480`).
- `Type::Record(_) => "Rec"` and `Type::Enum(_) => "Enum"` (`lib.rs:12472-12473`)
  collapse every structural record and enum to one string.

The monomorphization driver dedups by that symbol (`lib.rs:1713-1716`):

```rust
let sym = mangle_name(&name, &type_args);
if !emitted.insert(sym.clone()) {
    continue;
}
```

so the second instantiation is silently skipped and both call sites call the
first body.

**Repro** (`vyrn check` prints `ok`):

```vyrn
type OptInt64 = { a: Int64 }

fn dup<T>(x: T) -> Array<T> {
    let mut xs: Array<T> = []
    xs.push(x)
    return xs
}

fn main() -> Int64 {
    let o: Option<Int64> = Some(5)
    let r = OptInt64 { a: 9 }
    let xs = dup(o)
    let ys = dup(r)
    print("\{xs.length} \{ys.length}")
    print("\{ys[0].a}")
    let m = match xs[0] {
        Some(v) => v,
        None => -1,
    }
    print("\{m}")
    return 0
}
```

**Recorded output.**

```
$ vyrn run p5_mangle.vyrn          # interpreter
1 1
9
5
exit=0

$ vyrn build p5_mangle.vyrn -o p5.exe && ./p5.exe    # native
1 1
1948444778241
5
exit=0

$ vyrn build p5_mangle.vyrn --target wasm && wasmtime p5.wasm
1 1
9
5
exit=0
```

Three-way parity is broken and the wrong engine is the reference one: native
reads the one-word record `{ a: 9 }` through the `Option` instantiation's
body and answers stack garbage. A record with a `String` field would make the
same read a wild pointer dereference.

**The emitted IR shows the collision directly** (`vyrn emit-ir`):

```llvm
%t7  = call { ptr, i64, i64 } @vyrn_dup__OptInt64({ i1, i64, i64 } %t6)
%t10 = call { ptr, i64, i64 } @vyrn_dup__OptInt64({ i64 } %t9)
...
define { ptr, i64, i64 } @vyrn_dup__OptInt64({ i1, i64, i64 } %arg0) { ... }
```

One `define`, two call sites with different argument types. LLVM's call
instruction carries its own function type, so clang builds this without a
diagnostic and the mismatch becomes undefined behaviour at run time — which is
why the failure is silent.

**The defect is already documented — in the other backend.** The direct wasm
backend keys its instantiation cache structurally, and its comment says why
(`direct.rs:712-721`):

> Deliberately the type arguments and targets THEMSELVES rather than a mangled
> name: `mangle_name` is the textual backend's symbol and it is not injective
> (every record mangles as `Rec`), so two distinct specializations can produce
> one symbol and the textual driver's `emitted.insert(sym)` silently skips the
> second.

That is a correct description of a Critical defect in the sibling backend,
recorded as a design note for this one. The wasm engine is immune (verified
above); the interpreter never mangles; only the reference native path is wrong.

**Why the gate does not catch it.** The parity corpus would need a program
whose user-declared type name collides with a builtin mangle prefix — nothing
in `examples/` does, and nothing makes the collision a compile error.

**Fix, one sentence.** Key `emitted` on `(name, type_args)` as the direct
backend already does — the symbol can stay ugly as long as the dedup does not
trust it — or make `mangle_ty` injective (length-prefixing or hashing the
argument list) and keep the symbol as the key.

---

(Remaining lenses follow in the next commit.)
