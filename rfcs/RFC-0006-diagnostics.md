# RFC-0006 — Diagnostics

- **Status:** Draft
- **Depends on:** RFC-0001, RFC-0004

---

## Summary

Diagnostics are a **first-class language feature**, not an afterthought. The
compiler is a *teacher, not a gatekeeper*. Because the compiler already tracks
capabilities (RFC-0004) internally, it can present conflicts in terms of the
programmer's intent — this is the candidate *signature experience* of Vela.

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

### After (Vela, intent-first)
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
