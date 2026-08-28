# A surface builtin shadowed in one module is shadowed in every module

**FIXED 2026-08-25.** Pinned by `examples/shadowbuiltin.vyrn`, which is in the
three-way corpus and fails on the old compiler with the two diagnostics below.

Two things this file got wrong, both found by fixing it:

1. **It is not about exports.** The report below describes an `export fn rawAt`,
   which is how it was met. A PRIVATE `fn raw` does it too — any declaration of
   the name, anywhere in the linked program.
2. **The fix is not only "scope the two lookups".** The checker cannot answer
   the question alone: `Program::imports` is consumed by the loader and the
   checker never sees it, so the checker cannot tell a module that IMPORTED
   `render` from one that merely shares a program with a module declaring it.
   The first attempt scoped the checker to each module's own declarations and
   broke two existing loader tests, where a root legitimately imports a
   function called `render`. An import shadows as surely as a declaration.

What shipped: the loader records `(module, name)` for every module that can SEE
a declaration of a surface builtin — its own or an imported one — on
`Program::surface_shadows`, and the checker asks that. The four names live in
one table, `ast::SURFACE_BUILTINS`, which the loader, the checker and the
interpreter all read.

The interpreter's copy of the test is still flat, deliberately, and says why at
the line: the four are legal only inside a `gen fn`, a `gen fn` runs in the
generation sandbox, and the sandbox's program is the generator module's own
closure. If that ever widens, `examples/shadowbuiltin.vyrn` is what will say so.

The original report follows, as written.

## What happened

`site/app/repo.vyrn` gained a four-line function:

```vyrn
export fn rawAt(gitRef: String, path: String) -> String {
    return ghRaw(gitRef, path)
}
```

After that, `vyrn check site/app/repo.vyrn` failed:

```
N:/wt-bytefix/std/vyx.vyrn:1885:0: function `rawAt` is defined in `site/app/repo.vyrn` but not imported here — add it to an `import { .. } from` list
```

`std/vyx.vyrn:1885` is `return render(rawAt(text, src, line, col))`. That call
means the RFC-0054 code-quote builtin, which takes four arguments. It does not
mean the two-argument function that had just been written in a different module,
in a different directory, that `std/vyx.vyrn` neither imports nor knows about.

## Why

`render`, `rawAt`, `raw` and `lex` are the four SURFACE builtins. RFC-0054 left
them unreserved on purpose, and the comment says why:

```rust
// The surface builtins (`render`/`rawAt`/`raw`/`lex`) are common words, so
// they are NOT reserved: a user function or binding of the same name wins
// (resolved below), and the builtin only applies when nothing shadows it.
```
— `compiler/vyrn-frontend/src/checker.rs:6445`

The test for "nothing shadows it" is `self.sigs.get(name).is_none()`, and the
interpreter's is `self.funcs.contains_key(name.as_str())`
(`compiler/vyrn-frontend/src/interp.rs:4923`). Both tables are the LINKED
program's, flat across every module. So the question the code asks is "does any
module anywhere in this program define `rawAt`", when the question the comment
describes is "does anything in scope HERE define `rawAt`".

One module opting out of a builtin opts every module out of it. The arity did
not even have to match.

## Why it is worth fixing

The intent is already written down and the implementation does something wider
than the intent. That is the whole finding; nothing here is a judgement call
about what the design should be.

Three things follow from the current behaviour:

- A leaf module cannot be written in isolation. Whether `fn raw(...)` is legal
  depends on every other module the eventual program links, including std.
- The blast radius grows with the program. A site linking 40 modules has 40
  chances to disable a builtin std relies on.
- The diagnostic names the two files and still reads as nonsense, because it
  reports the collision from the victim's side. `std/vyx.vyrn` is told to import
  a function it must not call.

These are the common words the RFC called them. `render` in particular is a
plausible name for a function on a site that renders pages.

## What would fix it

Scope the shadowing test to the module the call is in, in both engines: the
call's own module's declarations plus what that module imports, not the linked
program's flat table. The two sites are the ones quoted above.

## What is NOT claimed

- No miscompile is demonstrated. Every case found so far is a check-time
  failure, which is the safe direction.
- Whether a module SHOULD be able to shadow a surface builtin at all is a
  separate question this does not answer. Reserving the four names would also
  close the hole, and would be a language change rather than a fix.
- The count of affected names is exactly four. Every other builtin is either
  `@`-prefixed and unspellable or genuinely reserved.

## Meanwhile

`site/app/repo.vyrn` names its function `rawFile` and carries a comment saying
why it is not `rawAt`.
