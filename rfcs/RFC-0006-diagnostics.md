# RFC-0006 — Diagnostics

- **Status:** Draft
- **Depends on:** RFC-0001, RFC-0004

---

## Summary

Diagnostics are a **first-class language feature**, not an afterthought. The
compiler is a *teacher, not a gatekeeper*. Because the compiler already tracks
capabilities (RFC-0004) internally, it can present conflicts in terms of the
programmer's intent — this is the candidate *signature experience* of Vyrn.

> **First instance (v0.1).** The `consume` move-checker (RFC-0004) already
> follows this format — it names the intent, locates the consumer, and explains
> the rule:
> ```
> error: line 6: `tok` is used here but was already consumed by `use_up(..)` on line 5
>   (a `consume` parameter takes ownership; the value can't be used afterward)
> ```
> Suggested fixes and editor/hover integration (below) remain to be built.

---

## 1. The format

Every capability/type error answers four questions, in order:

1. **What did you ask for?** (the operation and the capability it needs)
2. **Why can't it happen now?** (the conflicting state)
3. **Who is responsible?** (the specific code holding the conflicting capability,
   with a location)
4. **How do you fix it?** (concrete, ranked suggestions)

### Before (Rust-style, mechanism-first)
```
error[E0502]: cannot borrow `user` as mutable because it is also
              borrowed as immutable
```

### After (Vyrn, intent-first)
```
error: rename(user) needs to MODIFY `user`

  23 |   print(user)
     |         ---- `user` is being READ here
  24 |   rename(user)
     |          ^^^^ MODIFY not available while a READ is active

  The read from print(user) is still active on line 23.
  `user` becomes modifiable again after line 23.

  Fixes:
    • move rename(user) so it runs before print(user)
    • clone user:            rename(user.clone())
    • let print consume it:  change print(user: read User)
                                    to print(user: consume User)
```

## 2. Principles

- **Name intent, not internals.** Say `MODIFY` / `READ` / `CONSUME`, never
  "mutable borrow" / "lifetime `'a`". The vocabulary matches RFC-0004's surface.
- **Always locate the other party.** A capability conflict has two sides; show
  both with source spans.
- **Always propose fixes.** Ranked, concrete, paste-able. If there are trade-offs
  (clone costs memory; `consume` changes the API), say so briefly.
- **Confirm expectations.** When code is *rejected*, the message should read as
  "here's the one thing blocking what you clearly intended," not "here is a rule
  you didn't know."
- **Explain the timeline.** Capabilities are state-dependent; say *when* the
  operation becomes available again ("after line 23").

## 3. Tooling surface (beyond the CLI)

The same capability information powers editor feedback, so most conflicts are seen
*before* compiling:

- **Hover** on a value shows its currently-available operations and any active
  restriction with its cause and its end point.
- **Inline** markers where a capability becomes temporarily unavailable.

```
user: User
  available: read, modify, consume, share
  (after line 23, while print(user) holds a read: modify unavailable)
```

This does not change semantics — only feedback. Many errors become
understandable, and fixable, before the user hits build.

## 4. Validation diagnostics (RFC-0003)

Refinement failures known at compile time report the predicate that failed and
the offending value:

```
error: Port(70000) is not a valid Port
  Port requires: value in 1..=65535
  70000 is greater than 65535
```

For runtime-checked construction, the diagnostic is about the *type of the
result* ("Email(input) returns Result<Email> because input is not a compile-time
constant"), nudging the programmer toward `?` or `match`.

## 5. Error recovery & accumulation (implemented)

One error should never hide the next. The pipeline accumulates rather than
aborting at the first problem, at three granularities:

- **Lexing** stops at the first illegal token (there is nothing reliable to
  resume from), so a lex error is reported alone.
- **Top-level recovery.** A malformed declaration is recorded, and the parser
  synchronizes to the next top-level starter (`fn`/`type`/`protocol`/`impl`/
  `let`/`test`/`logging`) at brace depth 0, then keeps going — so a broken `fn`
  does not hide a later broken `type`.
- **Statement-level recovery (implemented).** A statement that fails to parse
  *inside a body* is recorded and dropped, and the parser synchronizes to the
  next statement boundary — a token that starts a new source line at the block's
  brace depth, a `;` at that depth, or the block's closing `}` — then continues
  parsing the same body. Brace-depth tracking means a `{ .. }` inside the bad
  statement doesn't fool the resync. Several bad statements in one body each get
  their own diagnostic; recovery works inside nested blocks (`if`/`while`/
  `for`/`region`) too. Expression-internal errors are unaffected — they surface
  as that one statement's error.

  The payoff is editor-facing: because a body parse error now leaves a **usable
  (partial) AST**, `symbols::analyze` still indexes the file's symbols, tokens,
  and locals while you are mid-edit, so hover, outline, and completion keep
  working through a syntax error instead of blanking out. To avoid a cascade,
  the type-checker and move-checker are **skipped whenever any parse error
  exists** — with parse errors present the reported diagnostics are the parse
  errors only, never spurious "unknown name"/type-mismatch follow-ons on a
  half-formed tree. Once the source parses cleanly, every type/ownership error
  across all functions and types is reported in one pass.

---

## Open questions

- **Q1.** How much of the "timeline" (when a capability returns) can be computed
  cheaply for large functions without hurting compile times?
- **Q2.** Fix suggestions that edit signatures (`read → consume`) cross function
  boundaries — how aggressively should the compiler propose changes to *other*
  functions?
- **Q3.** Machine-applicable fixes (à la `rustfix`) from day one, or after the
  format stabilizes?
- **Q4.** Diagnostic output format for the LSP vs the CLI — shared structured
  representation with two renderers.

---

## Addendum (implemented) — the parser refuses source nested past 1024 levels

A recursive-descent parser turns nesting in the source into nesting on the Rust
stack, and a Rust stack overflow is a process abort: exit 127, a message from the
Rust runtime, no `file:line`, and nothing this RFC's machinery ever sees. An
audit reached it with 175,000 nested parentheses; 150,000 checked fine, so the
compiler both accepted and aborted on the same shape of input.

The threshold sits far above hand-written code, which is not the interesting
part. RFC-0010 fetches `github:` and `https:` modules, so the compiler parses
source the author did not write; and the LSP parses whatever is on disk, so one
pathological file took the editor down with it.

**The limit is 1024 levels.** One nested parenthesis, one prefix operator, one
nested type argument and one nested block are one level each. Past it the parser
reports an ordinary parse diagnostic, source-anchored like every other one:

```
deep.vyrn:2:1035: nesting exceeds 1024 levels
```

`vyrn check` exits 1, the code every other check failure uses.

The counter sits on the three recursive edges a file can drive without bound —
`unary` (which every expression recursion enters exactly once), `type_atom` and
`block` — and it is decremented on the error path as well as the success path,
because a declaration that fails to parse is recovered from and the next one must
not start at the depth this one left behind.

1024 was chosen from both ends. The corpus peaks in the tens, and every pass
downstream of the parser survives 1020 levels on all three backends — checked,
run, and compiled — so the limit refuses nothing the rest of the compiler could
have finished.
