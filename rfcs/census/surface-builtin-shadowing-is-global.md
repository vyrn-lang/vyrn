# A surface builtin shadowed in one module is shadowed in every module

Found while removing a duplicated repository slug, not looked for. One added
function broke a std module that does not import it.

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
