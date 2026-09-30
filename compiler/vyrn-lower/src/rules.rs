//! The rules the ownership and typed judgments state, one row each: the
//! sentence, then each way out after a `|`, with `{key}` holes the rule's
//! site fills. A site decides that its rule holds and calls [`say`].
//!
//! The kernel judges the flow rules on its solver: at a use (shape A), where
//! a scope ends (B), where edges join (C) and at a loop's back edge (D).
//! The flow-free rules (E) filter single rows: the kernel's at its use
//! sites, `typed`'s over every row including rows after an ended path, and
//! the builder's at the construct.

use vyrn_frontend::diagnostics::menu;
use vyrn_frontend::own::Exit;

/// The words of `rule`: its sentence and its ways out as a menu, each `{key}`
/// filled from `args`, and `{{` and `}}` as braces. A hole `args` lacks is a
/// defect in the rule's site, so it panics.
pub fn say(rule: &str, args: &[(&str, &str)]) -> String {
    let fill = |t: &str| {
        let (mut out, mut rest) = (String::new(), t);
        while let Some((head, tail)) = rest.split_once('{') {
            out.push_str(&head.replace("}}", "}"));
            if let Some(t) = tail.strip_prefix('{') {
                out.push('{');
                rest = t;
                continue;
            }
            let (key, after) = tail.split_once('}').expect("a rule's hole is closed");
            let v = (args.iter().find(|(k, _)| *k == key))
                .unwrap_or_else(|| panic!("the rule's hole `{key}` is not filled"));
            out.push_str(v.1);
            rest = after;
        }
        out + &rest.replace("}}", "}")
    };
    let mut parts = rule.split('|');
    let sentence = fill(parts.next().unwrap_or_default());
    menu(sentence, parts.map(fill))
}

/// Returns `msg`, which a refusal prints. A sentence quotes what the reader
/// wrote, so a compiler temporary (`@t1`, `@p3`) in backticks is a defect in
/// the site that named it; debug builds panic on one.
pub fn spoken(msg: String) -> String {
    debug_assert!(
        !msg.contains("`@"),
        "a refusal names a compiler temporary: {msg}"
    );
    msg
}

// Shapes A to D: the kernel's flow rules.

/// A use after a declared `consume`, a `drop`, or a linear value's take.
pub const CONSUMED: &str =
    "`{read}` is {what} here but was already consumed by {by} on line {l}{note}";
/// A use after any other take, at the take.
pub const MOVED: &str =
    "`{s}` was moved here into {by}\nline {here}: ... and `{s}` is {what} again \
                     here|`{s}.copy()` if both sides need a value";
/// A use after a placed release.
pub const RELEASED: &str = "`{s}` is {what} here after it was released";
/// A read of an alias whose place was written, at the write.
pub const ALIAS_READ: &str = "`{place}` is written here while `{s}` still reads out of it\nline \
                          {here}: ... and `{s}` is {what} again here|`{src}.copy()` on line \
                          {at}, so `{s}` is a value of its own";
/// An argument that reads a global the callee stores into.
pub const STATE_READ: &str = "`{place}` is written here while `{s}` still reads out of it\nline \
                          {here}: ... and `{s}` is {what} again here";
pub const WHOLE_WITH_HOLE: &str = "`{s}{path}` was taken out of `{s}` here\nline {here}: ... and \
                               `{s}` is used as a whole here, with the hole still in \
                               it|`{s}{path}.copy()` on line {l} if `{s}` is still needed \
                               whole|write `{s}{path}` back before this line";
pub const READ_IN_HOLE: &str = "`{s}{h}` was moved here into `consume`\nline {here}: ... and \
                            `{s}{h}` is used again here|`{s}{h}.copy()` if both sides need a \
                            value";
pub const STORE_UNDER_HOLE: &str = "`{s}{h}` was moved here into `consume`\nline {here}: ... and \
                                `{s}{path}` is written here, under the hole";
/// A payload binder handed on out of a type that declares `release`.
pub const SEALED_PAYLOAD: &str =
    "`{b}` may not be handed to a `consume` parameter: `{ty}` declares \
                              `release`, which reads it; consume `{m}` or copy `{b}`";
/// A written `drop` of a name with a hole.
pub const DROP_WITH_HOLE: &str = "`{s}` may not be dropped — `{s}{h}` was taken out of it on line \
                              {l}, and `drop` releases the whole binding|write `{s}{h}` back \
                              before the `drop`, so the binding is whole again|delete the \
                              `drop` — the parts still here are released when the block exits";
pub const RELEASED_WITH_HOLE: &str =
    "{info} is released whole although a `consume` took `{h}` out of it";
pub const RELEASED_AROUND: &str = "{info} is released around `{h}` on a path that did not take it";
pub const OVERWRITTEN: &str =
    "{info} is overwritten while still held — the old value is never released";
pub const RELEASED_BEFORE_STORE: &str =
    "{info} is released before a store although it holds nothing";
pub const HELD_AT_EXIT: &str = "{info} is still held at {exit} — no release is placed for it";
pub const HELD_AT_ARM_END: &str =
    "{info} is still held where its arm ends — no release is placed for it";
pub const JOIN_MOVED: &str =
    "`{s}` was moved here into {by} on one path and not on the other, and \
                          nothing releases it where the paths join";
pub const JOIN_RELEASED: &str =
    "`{s}` is released on one path and still held on another where the paths join";
pub const JOIN_HOLE: &str = "{info} has a `consume` hole on one edge of a join and not on another";
pub const LOOP_MOVED: &str =
    "`{s}` is consumed by {by} inside a loop, so it would be used again on the next iteration";
pub const LOOP_RELEASED: &str =
    "`{s}` is released inside a loop, so it would be used again on the next iteration";
pub const LOOP_BOUND: &str =
    "{info} is bound inside a loop that would use it again on the next turn";
pub const LOOP_HOLE: &str = "`{s}{h}` is consumed by `consume` inside a loop, so it would be used \
                         again on the next iteration|`{s}{h}.copy()` if both sides need a value";
pub const LOOP_HOLE_AT: &str =
    "{info} has a `consume` hole at a loop's back edge it did not have at entry";

/// A linear binding a path leaves held, or disposes of inside a loop or on
/// one branch only; said once, at the binding.
pub const NEVER_DISPOSED: &str = "`{s}` is {a} `{ty}` and is never disposed";
/// A linear binding used after its disposal, at the binding.
pub const DISPOSED_TWICE: &str = "`{s}` is {a} `{ty}` and is disposed more than once";
/// The note under either must-use row, by the row that obliges the type. A
/// stream's release is pushed by its own lowering, so `drop` on one reclaims
/// nothing; a declared type has no `close` and is not iterable unless it says
/// so.
pub const OWED_STREAM: &str = "a stream must be consumed with `for … in`, forwarded by returning \
                               it, or released with `close({s})` — on every path";
pub const OWED_DECLARED: &str = "`{ty}` declares `impl MustUse`, so a value of it must be handed \
                                 on by name — passed to a call, forwarded by returning it, or \
                                 released with `drop {s}` — on every path";
/// A container: the reader wrote `Array<Txn>` and the row is `Txn`'s.
pub const OWED_HELD: &str = "`{by}` declares `impl MustUse` and a `{ty}` holds one, so the \
                             container must be handed on by name — passed to a call, forwarded \
                             by returning it, or released with `drop {s}`, which releases each \
                             element — on every path";

/// Whether `message` is a must-use row's. A binding earns one per mistake
/// however many instances and paths reach it.
pub fn owed(message: &str) -> bool {
    [NEVER_DISPOSED, DISPOSED_TWICE].iter().any(|r| {
        r.rsplit('`')
            .next()
            .is_some_and(|tail| message.ends_with(tail))
    })
}

/// An exit, as [`HELD_AT_EXIT`] names it.
pub fn exit_words(e: Exit) -> &'static str {
    match e {
        Exit::Block => "the end of its scope",
        Exit::Return => "a `return`",
        Exit::Try => "a `?`",
        Exit::Break => "a `break`",
        Exit::Continue => "a `continue`",
        Exit::Scrutinee => "a scrutinee",
    }
}

// Shape E: the kernel's, at its use sites.

/// A `return` of a closure's captured binding.
pub const RETURNED_CAPTURE: &str = "`{s}` may not be returned from a closure — it is a captured \
                                    binding, and the closure's result is its caller's";
/// A `return` of a borrow from an `export extern fn`: the JS caller releases
/// what it is handed.
pub const RETURNED_TO_JS: &str = "`{s}` may not be returned from an exported function — it is \
                                  {what}, and the JS caller releases what it is handed";
pub const RETURNED_BORROW: &str = "`{s}` may not be returned — it is {what}, and a return is owned";
/// A take of a borrow; `{may_not}` names the taker.
pub const TAKEN_BORROW: &str = "{may_not} — it is {what}";
/// The ways out of [`TAKEN_BORROW`] and the `return` rules, one of which a
/// site appends after a `|`.
pub const COPY_TO_OWN: &str = "`{s}.copy()` if the value should own it";
pub const COPY_FOR_CALLER: &str = "`{s}.copy()` if the caller needs its own value";
pub const COPY_FOR_JS: &str = "`{s}.copy()` — an `export extern fn` owns its result";
pub const COPY_FROM_JS: &str = "`{s}.copy()` — an `export extern fn` may not take ownership of a \
                                String its JS caller releases";
pub const ESCAPING_CAPTURE: &str =
    "`{s}` may not be captured by a closure that outlives this call — it is {what}";
pub const RELEASED_UNOWNED: &str = "{info} is released although the body does not own it";
pub const STORE_RELEASES_NOTHING: &str =
    "a store into a place that owns heap releases nothing (line {l})";

// Shape E: the builder's, at the construct.

pub const ELEMENT_TAKEN: &str =
    "`{path}` may not be taken — an element is not a place a take reaches";
/// The way out of a take of an element, which a site appends after a `|`.
pub const SWAP_REMOVE: &str =
    "`{root}.swapRemove(..)` returns the element and leaves the container one shorter";
pub const LOOP_TAKES_NOTHING: &str = "`consume` here has nothing to take — the loop already owns \
                                      a container that is not a binding|drop the `consume`: the \
                                      elements are already owned";
pub const CONSUME_TAKES_NOTHING: &str = "`consume` here has nothing to take — the value is \
                                         already owned, so there is no place to leave a hole \
                                         in|drop the `consume`: the value is already owned";
pub const CONSUMED_BORROW: &str = "`{root}` may not be consumed — it is {what}";

// Shape E: `typed`'s, over every row.

pub const STORE_RULED: &str = "cannot mutate a field of `{n}` in place (its `where` invariant \
                               could be broken mid-update); rebuild it: `{name} = {n} {{ .. }}`";
pub const REMOVE_NOT_MUT: &str = "cannot `{op}` from `{name}` (declared without `mut`)";
pub const ASSIGN_NOT_MUT: &str = "cannot assign to `{name}` (declared without `mut`)";
pub const FIELD_NOT_MUT: &str = "cannot mutate a field of `{name}` (declared without `mut`)";
pub const STORE_NOT_MUT: &str = "cannot store into `{name}` (declared without `mut`)";
pub const OUTSIDE_LOOP: &str = "`{what}` outside a loop";
pub const DROP_MODULE_STATE: &str = "cannot `drop` module state `{name}` — it lives for the whole \
                                     module and is reclaimed at process exit";
pub const DROP_UNBOUND: &str = "`drop` of unbound variable `{name}`";
pub const DROP_TYPE_PARAM: &str = "cannot `drop` `{name}`: its type `{t}` is a type parameter, \
                                   so this body cannot know whether the rule below holds for the \
                                   instance — a plain record would be released here where \
                                   `drop` on it directly is refused. Release the value where its \
                                   concrete type is known, or `consume` the heap field and \
                                   `drop` that";
pub const DROP_NOT_HEAP: &str = "`drop` needs a heap value (a String, an Array, a Map, a Ref, or \
                                 an Option/Result carrying one, or a type declaring `impl \
                                 Owned`), but `{name}` is {t}";
