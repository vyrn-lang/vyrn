# std/contract

std/contract -- check a module against a module contract.

`checkContract` compares a `contract` declaration with a module's
`moduleInterface` and returns `Issue`s:

    import { checkContract } from "std/contract"

    gen fn pages(dir: String) -> String {
        let iface = moduleInterface(pageFile)
        let issues = checkContract(iface, contractOf(Page))
        ...
    }

The compiler knows only the declaration form, so a third-party generator can
declare its own contract. Every condition is reported:

  - required member absent            -> `contract.missing`
  - member type mismatch              -> `contract.type`
  - unknown export, close to a member -> `contract.unknown.didYouMean`
  - unknown export, not close         -> `contract.unknown`
  - open-rule shape mismatch          -> `contract.open`

A module exports only functions (every top-level `let` is
private), so a `let` member is satisfied by an accessor
(`export fn head() -> Head`). A member with a default (`fn head() -> Head =
noHead()`) is optional. A `fn` member name declared more than once has
alternative signatures; the module matches any one, the name is optional when
any alternative has a default, and `matchedMember` says which one matched:

    fn head() -> Head = noHead()
    fn head(d: T) -> Head
    fn head(p: P) -> Head
    fn head(p: P, d: T) -> Head

## checkContract

```vyrn
fn checkContract(iface: ModuleInterface, c: ContractInfo) -> Array<Issue>
```

One `Issue` per problem, in a fixed order: the contract's members in
declaration order, then the module's unrecognized exports in reflection order.
Empty when the module satisfies the contract.

## suppliesMember

```vyrn
fn suppliesMember(iface: ModuleInterface, c: ContractInfo, name: String) -> Bool
```

Whether `iface` exports member `name` at one of its declared shapes.

## matchedMember

```vyrn
fn matchedMember(iface: ModuleInterface, c: ContractInfo, name: String) -> Int64
```

The 0-based index of the alternative of member `name` the module supplies, in
declaration order, or `0 - 1` when it supplies none. Compares in place, with
no `Export` list built.
