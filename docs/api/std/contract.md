# std/contract

std/contract — check a module against a module contract (RFC-0071).

A `contract` declaration states which exports a module may have, with their
types, optionality and documentation. `checkContract` compares one against a
module's `moduleInterface` reflection and returns RFC-0009 `Issue`s, so a
generator replaces name-hunting and source-scanning with one call:

    import { checkContract } from "std/contract"

    gen fn pages(dir: String) -> String {
        let iface = moduleInterface(pageFile)
        let issues = checkContract(iface, contractOf(Page))
        ...
    }

Everything here is ordinary library code over two ordinary records. The
compiler knows the *declaration form* and nothing else — which is what lets a
third-party generator declare its own contract and get the same behaviour
without a compiler change.

The five conditions RFC-0071 enumerates, all reported and none silent:

  - required member absent            -> `contract.missing`
  - member type mismatch              -> `contract.type`
  - unknown export, close to a member -> `contract.unknown.didYouMean`
  - unknown export, not close         -> `contract.unknown`
  - open-rule shape mismatch          -> `contract.open`

**The export surface this reads.** `ModuleInterface` reflects a module's
exported FUNCTIONS (RFC-0021 / RFC-0031). `export let` does not exist and is
not coming: RFC-0029 makes every top-level `let` module-private, and a
contract member is satisfied by the accessor function that rule already
prescribes (`export fn head() -> Head`) — never by an `export let`, which
does not exist. The `let` member form stays in the grammar, so the grammar
needs no reopening if that rule ever changes.

**Optionality.** Either member form may carry a default (`fn head() -> Head =
noHead()`), which makes it optional: the module may omit the export and the
generator uses the default. A member without one is required.

**Alternative signatures (RFC-0071 M2b).** A `fn` member NAME may be declared
more than once; the repeats are alternatives, and the module satisfies the
member by matching any ONE of them:

    fn head() -> Head = noHead()
    fn head(d: T) -> Head
    fn head(p: P) -> Head
    fn head(p: P, d: T) -> Head

A name is optional when ANY of its alternatives carries a default, because
optionality is a property of the export being absent and an absent export is
absent at every shape. `matchedMember` reports WHICH alternative an export
matched, so a generator reads a page's shape off the declaration instead of
scanning its body — which is the whole point of the RFC.

## checkContract

```vyrn
fn checkContract(iface: ModuleInterface, c: ContractInfo) -> Array<Issue>
```

Check `iface` (a module's reflected surface) against contract `c`, returning
one RFC-0009 `Issue` per problem — in a fixed order: the contract's members
in declaration order first, then the module's unrecognized exports in
reflection order. Deterministic, because a generator bakes these into a
diagnostic and byte-stability is the whole game.

An empty result means the module satisfies the contract.

## suppliesMember

```vyrn
fn suppliesMember(iface: ModuleInterface, c: ContractInfo, name: String) -> Bool
```

Whether `iface` supplies contract member `name`: exported, and matching the
member's declared shape.

This is the contract-driven replacement for a generator's name hunt. `std/ui`
used to ask `uiHasFn(iface, "load")` — a literal string with nothing behind
it — and now asks whether the module supplies the `data` member of the
contract it is checked against, which is the same question with a declaration
standing behind every part of it.

## matchedMember

```vyrn
fn matchedMember(iface: ModuleInterface, c: ContractInfo, name: String) -> Int64
```

WHICH alternative signature of member `name` the module supplies: the 0-based
index among that member's alternatives in declaration order, or `0 - 1` when
the module does not supply it at all (RFC-0071 M2b).

This is how a generator reads a module's SHAPE off declarations. `std/ui` used
to learn whether a page was lazy by scanning `data`'s body for a `lazy(…)`
call and whether its head could see the data by parsing a `head { … }` block
out of source text. Both are now questions about which declared signature the
page chose, answered here — and a signature is a thing the checker checks, the
LSP completes, and a typo breaks loudly.

A member with one signature answers `0` (supplied) or `0 - 1` (not), so a
caller that only wants presence should keep using `suppliesMember`.

One name query reads IN PLACE: the member rows and the reflected function
are compared where they live, and nothing is materialized — this was the
std-quality census's last pattern-2 payer, a whole `Export` surface built
(every spelling copied) to read one row of it. `checkContract` keeps
`moduleExports`, because a full check reads every row it builds.
