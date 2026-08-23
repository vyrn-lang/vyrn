# A4 — Reactivity in Vue and Nuxt, and what Vyrn does instead

A census of the reactivity model in Vue 3 and Nuxt, the failure classes users
hit, and how Vyrn's UI layer relates to each one. Every external claim cites a
URL. Every repository claim cites `path/to/file:LINE`. Performance claims cite a
command and its output, or say `NOT MEASURED`.

---

## Part one — Vue and Nuxt, from the source and the documentation

Vue 3 reactivity is a runtime system. It intercepts object property access with
ES Proxies and `ref` `.value` access with getter/setters. Each read records the
running effect as a subscriber. Each write notifies those subscribers
(https://vuejs.org/guide/extras/reactivity-in-depth.html).

### The reactive primitives

**`ref`.** `ref(value)` returns a mutable object whose only reactive surface is
`.value`. Reading `.value` calls `Dep.track()`. Assigning `.value` calls
`Dep.trigger()` when the new raw value differs. It exists because JavaScript
gives no way to intercept reads and writes of a primitive local variable. `ref`
boxes the primitive so access goes through a property. When the inner value is a
plain object, `ref` stores `toReactive(value)`, so the object is made deeply
reactive on first access
(https://vuejs.org/api/reactivity-core.html#ref,
https://raw.githubusercontent.com/vuejs/core/main/packages/reactivity/src/ref.ts).

**`reactive`.** `reactive(obj)` returns a deep ES Proxy of `obj`. The `get` trap
calls `track(target, GET, key)` before returning. The `set` trap mutates then
calls `trigger(target, ADD|SET, key, ...)`. Deepness comes from the `get` trap:
when a returned value is an object, it is wrapped with `reactive()` again, so
nested property access is also trapped. `reactive()` only converts object types
whose `Object.prototype.toString` tag maps to `Object`/`Array` (COMMON) or
`Map`/`Set`/`WeakMap`/`WeakSet` (COLLECTION). Other tags return unchanged
(https://vuejs.org/api/reactivity-core.html#reactive,
https://raw.githubusercontent.com/vuejs/core/main/packages/reactivity/src/baseHandlers.ts,
https://raw.githubusercontent.com/vuejs/core/main/packages/reactivity/src/reactive.ts).

**`computed`.** `computed(getter)` returns a `ComputedRefImpl`. It is lazy: the
getter runs only on first `.value` read, and again only when a dependency
changes and someone reads it again. Caching uses a `globalVersion` counter
incremented on every reactive change, and per-link `version` numbers. A computed
is itself a `Subscriber`. When its getter runs, `activeSub` is the computed, so
its deps link back to it. A computed with no subscribers unsubscribes from its
deps so it and its value can be garbage-collected
(https://vuejs.org/api/reactivity-core.html#computed,
https://raw.githubusercontent.com/vuejs/core/main/packages/reactivity/src/computed.ts).

**`watch`.** `watch(source, cb, options?)` is lazy by default. The source is a
ref, a getter, a reactive object, or an array of these. A `ReactiveEffect`
tracks the source's getter. When the source changes, the effect's `scheduler`
runs instead of `run()`, and the scheduler queues the callback as a job. Timing
is controlled by `flush: 'pre' | 'post' | 'sync'` (default `'pre'`). The
callback receives an `onCleanup` register function. The cleanup runs before the
next callback run or on stop
(https://vuejs.org/api/reactivity-core.html#watch,
https://raw.githubusercontent.com/vuejs/core/main/packages/reactivity/src/effect.ts).

**`watchEffect`.** `watchEffect(fn)` runs `fn` immediately, tracks every
reactive read inside it, and re-runs whenever any tracked dep changes. It uses
the same `ReactiveEffect` + scheduler + `flush` options as `watch`. The returned
handle is callable to `stop()`, and exposes `pause()`/`resume()`/`stop()`
(https://vuejs.org/api/reactivity-core.html#watcheffect).

**`shallowRef` / `shallowReactive`.** `shallowRef(value)` stores the value
as-is. Only `.value` access is tracked. Deep mutations to `.value.foo` do not
trigger. `triggerRef(ref)` force-triggers the ref's dep after a manual deep
mutation. `shallowReactive(obj)` proxies only root-level properties. Nested
objects are not converted and their mutation is not reactive
(https://vuejs.org/api/reactivity-advanced.html#shallowref,
https://vuejs.org/api/reactivity-advanced.html#shallowreactive,
https://raw.githubusercontent.com/vuejs/core/main/packages/reactivity/src/ref.ts).

**`toRefs`.** `toRefs(reactiveObj)` returns a plain object whose own properties
are `toRef()` refs pointing at the source properties. Each ref's getter reads
`source[key]` (so reading tracks the source property) and its setter writes
`source[key]` (so writing triggers it). This preserves reactivity across
destructuring, because each destructured binding is a ref, not a snapshot.
`toRefs` only emits refs for properties enumerable at call time
(https://vuejs.org/api/reactivity-utilities.html#torefs).

**`markRaw`.** `markRaw(obj)` stamps `obj[ReactiveFlags.SKIP] = true` and
returns the object itself. `createReactiveObject` checks this flag and returns
the raw object unchanged. A marked object is never proxied, even when nested
inside a reactive object. The docs flag an identity hazard: the opt-out is
root-only, so a nested non-marked raw object re-acquired through a reactive
parent comes back proxied
(https://vuejs.org/api/reactivity-advanced.html#markraw,
https://raw.githubusercontent.com/vuejs/core/main/packages/reactivity/src/reactive.ts).

### How the dependency graph is built and when it is torn down

The graph is built around a global `activeSub` (the docs call it `activeEffect`)
and a per-property `Dep`. `ReactiveEffect.run()` sets `activeSub = this` around
`fn()`, runs `prepareDeps` (marks existing links `version = -1`), then runs
`cleanupDeps` to drop links still at `-1` — deps not re-read this run are
unsubscribed. `track(target, type, key)` finds or creates the `Dep` at
`targetMap.get(target).get(key)` and calls `dep.track()`, which adds a `Link`
between the `Dep` and `activeSub` to two doubly-linked lists
(https://raw.githubusercontent.com/vuejs/core/main/packages/reactivity/src/effect.ts,
https://raw.githubusercontent.com/vuejs/core/main/packages/reactivity/src/dep.ts).

Every `ReactiveEffect` registers itself in `activeEffectScope.effects` at
construction. `EffectScope.stop()` iterates `effects` calling `effect.stop()`
(which `removeSub`s every link and runs the cleanup), runs `cleanups`
(registered via `onScopeDispose`), and recurses into child scopes. Each
component's `setup()` runs inside an effect scope, so `onScopeDispose` covers
component teardown. `watch`/`watchEffect` handles and the `effect()` runner
expose `stop()` for manual teardown. A stopped effect sets `~ACTIVE` and
unsubscribes from all deps, after which writes no longer re-run it
(https://vuejs.org/api/reactivity-advanced.html#onscopedispose,
https://raw.githubusercontent.com/vuejs/core/main/packages/reactivity/src/effectScope.ts,
https://raw.githubusercontent.com/vuejs/core/main/packages/reactivity/src/effect.ts).

### What triggers a re-render, and what batches those triggers

A component's render function runs inside a `ReactiveEffect`. Any reactive read
during render tracks the render effect as a subscriber. A later write to those
deps calls `dep.trigger()`, which calls the render effect's `scheduler`. That
scheduler enqueues a component-update job via `queueJob`.

`queueJob` dedupes by a `QUEUED` flag and inserts by component `id` so parents
update before children. `queueFlush` schedules `flushJobs` on a microtask
(`Promise.resolve()`). `flushPreFlush` runs `PRE` watcher jobs before the
component update. `flushPostFlushCbs` runs post jobs after. `nextTick(fn)`
returns `currentFlushPromise || resolvedPromise` so user code runs after the
current flush. This is the batching layer: multiple synchronous mutations
coalesce into one render
(https://raw.githubusercontent.com/vuejs/core/main/packages/runtime-core/src/scheduler.ts).

### Where the Proxy-based tracking cannot see a change

**(a) Array index assignment.** On a reactive array, `arr[i] = x` is tracked:
the `set` trap calls `Reflect.set` then `trigger(target, ADD|SET, key, ...)`,
and for integer keys also runs the `ARRAY_ITERATE_KEY` dep. On a plain
(non-reactive) array there is no proxy, so nothing is tracked. Length writes
trigger deps for keys `>= newLength`, `length`, and `ARRAY_ITERATE_KEY`. Array
identity-sensitive methods (`indexOf`, `includes`) are patched in
`arrayInstrumentations` so they track and operate on the raw array
(https://raw.githubusercontent.com/vuejs/core/main/packages/reactivity/src/baseHandlers.ts,
https://raw.githubusercontent.com/vuejs/core/main/packages/reactivity/src/dep.ts).

**(b) `Map` and `Set`.** Vue uses collection handlers that wrap the mutating
methods. `get`, `has`, `size`, `forEach`, and iteration all call `track`. `add`,
`set`, `delete`, and `clear` call `trigger` with `ADD`/`SET`/`DELETE`/`CLEAR`.
So `map.set(k, v)` and `set.add(v)` are tracked when the Map/Set is reactive.
`WeakMap`/`WeakSet` are handled but their non-iterable nature limits tracking to
`get`/`has`/`add`/`set`/`delete`
(https://raw.githubusercontent.com/vuejs/core/main/packages/reactivity/src/collectionHandlers.ts).

**(c) Class instances.** A plain class instance is not reactive until passed
through `reactive()`. For a typical class instance, `targetTypeMap` returns
`COMMON` and `reactive()` wraps it with the base handlers. If the class defines
a custom `[Symbol.toStringTag]`, `targetTypeMap` returns `INVALID`, so
`reactive()` returns the instance unchanged and property access is not tracked.
Instances of built-ins like `Date`, `RegExp`, `Error`, `Promise` map to
`INVALID` and are not deeply converted
(https://raw.githubusercontent.com/vuejs/core/main/packages/reactivity/src/reactive.ts).

**(d) Getters.** Accessing a `computed`'s `.value` calls `this.dep.track()`
(linking the reader) and `refreshComputed(this)` (running the getter if dirty).
Object getters defined via `get` accessor properties are transparent to the
proxy `get` trap: `Reflect.get` invokes the accessor, and `track(target, GET,
key)` runs regardless. A getter that reads no reactive state has no downstream
deps and will not re-run on mutation
(https://raw.githubusercontent.com/vuejs/core/main/packages/reactivity/src/computed.ts,
https://raw.githubusercontent.com/vuejs/core/main/packages/reactivity/src/baseHandlers.ts).

### Server-side rendering and hydration

Vue SSR renders the same component tree to an HTML string on the server. The
server calls `renderToString(app)` from `vue/server-renderer`. The client calls
`createSSRApp(...)` and `app.mount('#app')`. Mounting an app created with
`createSSRApp` performs hydration instead of creating new DOM nodes
(https://vuejs.org/guide/scaling-up/ssr.html#client-hydration,
https://vuejs.org/api/ssr.html#rendertostring).

Vue itself does not define a built-in `__INITIAL_STATE__` convention. The SSR
guide leaves state transfer to the application or a higher-level framework. The
documented pattern is: create a new app instance (with router and stores) per
request, expose the store, serialise it to JSON, inline it in the page HTML, and
read it back on the client before mount
(https://vuejs.org/guide/scaling-up/ssr.html#cross-request-state-pollution).

Reactivity is disabled on the server for performance. Only `beforeCreate` and
`created` (or the `setup()` root) run on the server. `onMounted`, `onUpdated`,
and `onUnmounted` do not run on the server
(https://vuejs.org/guide/scaling-up/ssr.html#reactivity-on-the-server).

A hydration mismatch happens when the DOM structure produced by the server
render does not match the DOM the client app expects. The guide lists three
common causes: invalid HTML nesting that the browser parser corrects;
non-deterministic render output (`Math.random()`, `Date.now()`); and timezone
differences. The fixes are to render non-deterministic output only on the client
(`v-if` plus `onMounted`, or `<ClientOnly>`), or to use a seeded random
generator and carry the seed in the serialised state. In development, Vue emits
`Hydration text mismatch in <node>` warnings. In production, it prints
`Hydration completed but contains mismatches.`. On a mismatch, Vue discards the
server nodes and mounts the client nodes, which costs render performance
(https://vuejs.org/guide/scaling-up/ssr.html#hydration-mismatch,
https://github.com/vuejs/core/blob/main/packages/runtime-core/src/hydration.ts).

### Nuxt on top

**`useState`.** `useState(key, init)` creates a reactive value keyed by a
string. The value is preserved across server rendering and client hydration,
and is shared across components that use the same key. The source prefixes the
key with `$s`, then reads a reactive ref from `nuxtApp.payload.state[key]` via
`toRef(nuxtApp.payload.state, key)`. The JSDoc states the intent: "Create a
global reactive ref that will be hydrated but not shared across ssr requests."
The value must be JSON-serialisable: no classes, functions, or symbols
(https://nuxt.com/docs/4.x/api/composables/use-state,
https://github.com/nuxt/nuxt/blob/main/packages/nuxt/src/app/composables/state.ts).

**`useAsyncData` / `useFetch`.** `useAsyncData` runs an async handler and adds
the resolved data to the Nuxt payload so the client does not re-fetch it during
hydration. The `server` option (default `true`) controls whether the handler
runs on the server. The default `getCachedData` reads
`nuxtApp.payload.data[key]` while hydrating. `useFetch` is a typed wrapper
around `useAsyncData` and `$fetch`. It auto-generates a key from the URL,
options, and call site, and stores the response in the same payload
(https://nuxt.com/docs/4.x/api/composables/use-async-data,
https://nuxt.com/docs/api/composables/use-fetch).

**Payload serialisation.** Nuxt carries server state to the client in a payload
object on `nuxtApp.payload`. The documented keys are `serverRendered` (boolean),
`data` (keyed results of `useFetch`/`useAsyncData`), and `state` (keyed values
of `useState`). The payload is serialised with the `devalue` library. On the
client, `getNuxtClientPayload()` reads the `<script id="__NUXT_DATA__">`
element, parses its text content with `devalue`'s `parse()` using registered
revivers, and merges it with `window.__NUXT__`. Custom types cross through
`definePayloadReducer`/`definePayloadReviver` pairs. A non-serialisable value
reaching the payload throws `Cannot stringify arbitrary non-POJOs`
(https://nuxt.com/docs/4.x/api/composables/use-nuxt-app#payload,
https://github.com/nuxt/nuxt/blob/main/packages/nuxt/src/app/composables/payload.ts).

**The shared-state-between-requests hazard.** On the server, JavaScript modules
initialise once at boot and are reused across requests. Any state declared at a
module's root scope becomes a singleton shared by every request. Mutating that
singleton with one user's data can leak it to another user's request. Vue calls
this "cross-request state pollution"
(https://vuejs.org/guide/scaling-up/ssr.html#cross-request-state-pollution).

The Nuxt state-management docs give the concrete rule: never declare
`const state = ref()` outside `<script setup>` or `setup()`. The prescribed
replacement is `const useX = () => useState('x')`. Nuxt mitigates the hazard for
`useState` by tying state to the per-request Nuxt context. `useState` reads from
`nuxtApp.payload.state`, and `nuxtApp` is created fresh for each request. The
mitigation is not automatic for all state. Module-level `ref`/`reactive` still
leaks, and Pinia on the server has the same singleton problem unless a new Pinia
instance is created per request
(https://nuxt.com/docs/getting-started/state-management#best-practices,
https://pinia.vuejs.org/ssr/).

A second mechanism compounds the hazard: effect scopes created during an SSR
render are not always stopped when the render ends. A Nuxt issue traces retained
`EffectScope` objects to top-level `await` in `<script setup>` that never
re-unsets `currentInstance`
(https://github.com/nuxt/nuxt/issues/33644,
https://github.com/nuxt/nuxt/pull/35011).

---

## Part two — how it fails

Six classes, each with at least one real report from a tracker or Stack
Overflow.

| class | what the user sees | root cause | the documented workaround | could a compiler have caught it |
| --- | --- | --- | --- | --- |
| Lost reactivity from destructuring | The view stops updating after a value change. No error prints. The app looks broken but stays silent. | `reactive()` returns a Proxy. Destructuring copies a primitive out of the Proxy into a plain local variable. The local has no link to the Proxy. A write to the local never reaches Vue's trigger path. The docs name this: a reactive object is "Not destructure-friendly" (https://vuejs.org/guide/essentials/reactivity-fundamentals.html). A filed issue reports the same trap with `defineProps` (https://github.com/vuejs/core/issues/11325). | Use `ref()` as the primary state API. Keep access on the Proxy: write `state.count`, not a destructured `count`. When splitting a reactive object, convert with `toRefs()` first. The `<script setup>` compiler rewrites destructured props to stay reactive (https://vuejs.org/guide/essentials/reactivity-fundamentals.html). | Yes. The compiler already detects destructuring of `defineProps` and rewrites it. The same static check can warn when a `reactive()` object is destructured into primitives without `toRefs()`. |
| Stale closure in a watcher | A `watch` callback runs once or runs with an old value, and never fires again when the source changes. | The user passes the value, not the source: `watch(props.selected, cb)`. The expression `props.selected` is evaluated once when `setup` runs. Vue receives a plain value, not a trackable source. It collects no dependencies. The accepted Stack Overflow answer explains this (https://stackoverflow.com/questions/59125857/how-to-watch-props-change-with-vue-composition-api-vue-3). | Pass a getter function: `watch(() => props.selected, cb)`. Or convert with `toRefs()` and pass the resulting ref (https://vuejs.org/api/reactivity-core.html#watch). | Yes, in the common case. A compiler can warn when the `watch` source is a member access on a reactive or props object, not wrapped in an arrow function, and not a `ref`. |
| Reactive object leaking between server requests | One visitor sees another visitor's data. Memory grows with each request until the server runs out of memory. Users report the heap climbing from tens of megabytes to over a gigabyte, then OOM crashes (https://github.com/nuxt/nuxt/issues/33644). | A `ref()` or `reactive()` is declared at module top level, outside `setup()`. The module loads once at boot. The same singleton is reused for every request (https://vuejs.org/guide/scaling-up/ssr.html#cross-request-state-pollution). A second mechanism: effect scopes created during SSR are not stopped when the render ends (https://github.com/nuxt/nuxt/pull/35011). | Do not declare `const state = ref()` outside `setup`. Put shared state behind `useState('key', init)`. For app stores, create a fresh store per request (https://nuxt.com/docs/getting-started/state-management). | Yes for the singleton case. A compiler can warn when `ref()`, `reactive()`, or `useState()` is called at module top level. No for the effect-scope leak, which depends on async context and runtime disposal. |
| Hydration mismatch from a non-deterministic render | The console prints `[Vue warn]: Hydration node mismatch` or `Hydration completed but contains mismatches.`. The server HTML and the client HTML differ. Vue discards the server nodes and remounts. | The render output depends on a value that differs between server and client: `Math.random()`, `Date.now()` in different timezones, or client-only data like `window` (https://vuejs.org/guide/scaling-up/ssr.html#hydration-mismatch). A tracker report describes mismatches that appear "only at certain times / dates" (https://github.com/vuejs/core/issues/8900). | Gate the non-deterministic part with `v-if` plus `onMounted`, or use `<ClientOnly>`. Use a seeded generator and share the seed through serialised state. From Vue 3.5, mark unavoidable mismatches with `data-allow-mismatch` (https://vuejs.org/api/ssr#data-allow-mismatch). | Partially. A compiler can flag `Math.random()`, `Date.now()`, and `new Date()` inside the render path. It cannot detect timezone drift or external-data nondeterminism. |
| Infinite update loop | The console fills with `[Vue warn]: Maximum recursive updates exceeded in component <X>.` The page freezes or throws. | A reactive effect writes to the same reactive state it reads. Each write schedules the effect again. Reports trigger it by reading a reactive variable inside `onScopeDispose` (https://github.com/vuejs/core/issues/6538), or by a `v-if` reading a template ref that two elements share (https://github.com/vuejs/core/issues/12246). | Do not mutate the watched value inside its own watcher or computed. Read and write through separate refs. Vue caps the loop and emits the warning, but the cap is a safety net, not a fix. | No, not in general. The cycle spans arbitrary data flow and crosses the template, watchers, and lifecycle hooks. A static check cannot prove that an effect mutates its own dependency. |
| Memory leak from an effect never stopped | Memory grows over the life of the app or server. A heap snapshot shows retained `EffectScope` and watcher objects. In SSR the growth is per request (https://github.com/vuejs/core/issues/14706). | A `watch` or `watchEffect` is registered outside any component instance and outside an `effectScope`. With no active scope, Vue cannot auto-stop the effect. In SSR the unmount hooks never fire, so retention happens per request even inside a component (https://vuejs.org/guide/scaling-up/ssr.html#component-lifecycle-hooks). | Register `watch` and `watchEffect` inside `setup()`. For effects that must outlive a component, own them with an `effectScope()` and call `scope.stop()`. For SSR libraries, stop scopes explicitly at the end of render (https://github.com/vuejs/apollo/pull/1582). | Yes for the static case. A compiler can warn when `watch` or `watchEffect` is called with no active component scope and no enclosing `effectScope`. No for the SSR retention from lifecycle hooks that never fire. |

---

## Part three — Vyrn today

Sources read for this part: RFC-0026 (the UI layer), RFC-0067 (soft navigation),
RFC-0068 (validation UX), RFC-0069 (universal pages), RFC-0071 through RFC-0075
(module contracts, audience, symbol maps, protocol projections, streams),
`std/ui.vyrn`, `std/vyx.vyrn`, `std/html.vyrn`, `web/vyrn-dom.js`,
`web/vyrn-nav.js`, `examples/domdemo.vyrn`, `examples/bin/client/boot.vyrn`,
and `site/app/routes/**/*.vyx`.

### What is the unit of state in a Vyrn page?

A Vyrn client island holds its state in module-level `let mut` bindings. The
binding is root-only (RFC-0013). The examples make this visible:

```
let mut count: Int64 = 0
let mut typed: String = ""
let mut timerOn: Bool = true
```

(`examples/domdemo.vyrn:17-21`)

The bin client holds the draft form state the same way
(`examples/bin/client/boot.vyrn:23-34`). There is no `ref()` wrapper, no
`.value`, and no Proxy. State is a plain mutable binding accessed by name.

A Vyrn server page has no client-side state. The page is a pure function of its
props, which arrive through `load()` and the route params. The site's own pages
under `site/app/routes/**/*.vyx` are server-rendered and carry no client island
at all: `site/app/routes/index.vyx` exports `head()` and renders a template with
no `vyrnView`/`vyrnSubs`/`vyrnPatch` export; `site/app/routes/play.vyx:37`
loads a separate JS playground through a `data-widget="play"` hook, not through
the TEA architecture.

### What causes a re-render?

A DOM event fires. The host walks to the nearest `data-on-<event>` element and
invokes the exported extern handler by name. The handler mutates module state.
When the handler returns, the host calls `rerender()`
(`web/vyrn-dom.js:290-298`):

```
function invoke(handler, arg) {
    const fn = exports[handler];
    fn(String(arg));
    rerender();
}
```

`rerender()` calls `vyrnView()` (or `vyrnPatch()`), diffs the result, and
patches the DOM (`web/vyrn-dom.js:643-656`). There is no scheduler, no
microtask queue, and no batching. One event produces one re-render. The host
owns the loop (RFC-0013, cited at `rfcs/RFC-0026-ui.md:34-35`).

### Is there a dependency graph at all, or does the whole view recompute?

There is no dependency graph. The whole view recomputes. RFC-0026 states this
directly: "Vyrn has no stored closures (RFC-0023 refused that bill), no runtime
reactivity graph" (`rfcs/RFC-0026-ui.md:34-35`). The view function is pure
(`examples/domdemo.vyrn:100-111`). Every event calls it again from scratch:

```
fn view() -> Html {
    return el("main", [], [ ... ])
}
```

The host re-renders the whole tree per event. The diff absorbs it
(`rfcs/RFC-0026-ui.md:126-129`):

> v1 re-renders the whole tree per event; the diff absorbs it. (M5 replaces
> this loop, not the surface.)

The deferred M5 compiled reactivity is the planned escalation: the template
compiler would statically know which bindings read which store fields, and
compile a `patch(dirty)` that re-evaluates only affected bindings
(`rfcs/RFC-0026-ui.md:379-385`). This is not shipped.

### Where does the diff happen, and what does the host do with it?

Two modes, negotiated by whether the module exports `vyrnPatch`
(`web/vyrn-dom.js:279`):

**Full-view loop** (no `vyrnPatch`). The host calls `vyrnView()`, gets the full
JSON `Html` tree, parses it, and diffs it against the retained old tree in
JavaScript. `patchChildren` does positional diffing; `patchKeyed` does keyed
reconciliation by matching `data-key` values and moving DOM nodes with
`insertBefore` (`web/vyrn-dom.js:394-449`). The whole tree crosses the extern
boundary each event — O(tree).

**Patch protocol** (with `vyrnPatch`). The diff runs in wasm. `vyrnPatch()`
calls `view()`, diffs it against a retained `lastTree`, and returns only the
`PatchOp` stream (`examples/bin/client/boot.vyrn:119-123`):

```
export extern fn vyrnPatch() -> String {
    let next = view()
    let ops = diff(lastTree, next)
    lastTree = next
    return toJson(ops)
}
```

`diff` is a pure function in `std/html.vyrn:465-468` that produces a minimal,
ordered `PatchOp` stream. The host applies the ops naively in order
(`web/vyrn-dom.js:480-526`). Only the changes cross the extern boundary —
O(changes). The `view()` call itself is still O(tree); the protocol reduces the
wire cost, not the compute cost.

### How does state survive a soft navigation?

The wasm instance and its module state persist across soft navigations. The
island is re-mounted against the new DOM node, but the wasm instance is
untouched. RFC-0067 states this (`rfcs/RFC-0067-soft-navigation.md:42-44`):

> Client state across navs: module state in the wasm instance survives (that's
> a feature — drafts persist across a nav and back); per-page islands are
> re-mounted from the new DOM.

The host runtime implements this through an island registry
(`web/vyrn-nav.js:106-148`). An island is booted once. On every later nav where
the mount selector reappears, `remountIsland` calls `inst.mount(el)` — the same
instance re-attaches its view to the new node. A nav to a page without the
selector leaves the instance alive and unmounted, so its module state persists
until the mount returns (`web/vyrn-nav.js:140-146`).

`vyrn-dom.js`'s `remount(newEl)` tears down the old mount's DOM-bound state
(subscriptions, effects, delegated events) and rebuilds from the full
`vyrnView()` against the new element. The wasm instance is not touched
(`web/vyrn-dom.js:668-689`).

### What crosses the server-to-browser boundary, and in what format?

Three things cross, at three times:

**1. First load (SSR).** The server renders `toHtmlString(view())` — a full
HTML string (`std/html.vyrn:393-400`). The browser receives complete HTML. No
client reactivity is involved. This is the path the site itself uses.

**2. Client boot.** The browser instantiates the client wasm, calls
`vyrnView()`, and receives `toJson(view())` — the `Html` tree as JSON
(`web/vyrn-dom.js:639-641`). The host parses it and builds the DOM. There is no
hydration: the SSR'd first page is never re-rendered by the client
(`rfcs/RFC-0069-universal-pages.md:22-24`).

**3. Soft navigation.** The client renders the next page itself from its
compiled view function. It fetches only a JSON payload from the same URL with
`Accept: application/json` (`rfcs/RFC-0069-universal-pages.md:79`):

```
{ "page": "p/[id]", "title": "<rendered title>", "props": <load result> }
```

The payload assembly is in `std/ui.vyrn:454-455`:

```
export fn uiPayload(page: String, title: String, props: String, params: String) -> String {
    return "{\"page\":" + toJson(page) + ",\"title\":" + toJson(title) + ",\"props\":" + props + ",\"params\":" + params + "}"
}
```

The client's `renderPage(payloadJson)` dispatches on `page`, decodes `props`
through the wire codec, calls the view function, and returns the `Html` tree as
JSON for `vyrn-dom` to paint (`std/ui.vyrn:2874-2876`). No reactive state
crosses the boundary. The payload carries data, not state. The client starts
fresh from `vyrnView()` on each island mount.

---

## Part four — the comparison

One row per failure class from Part two.

| failure class | can Vyrn have it | why | evidence |
| --- | --- | --- | --- |
| Lost reactivity from destructuring | IMPOSSIBLE BY CONSTRUCTION | Vyrn has no reactivity proxies. State is module-level `let mut` bindings accessed by name. The view function reads the binding directly. There is no reactive object to destructure, and no tracking to lose. The whole view recomputes on every event, so a binding read is always live. The language rule: RFC-0026 states "no runtime reactivity graph" (`rfcs/RFC-0026-ui.md:34-35`), and v1 re-renders the whole tree per event (`rfcs/RFC-0026-ui.md:126-129`). State is plain module bindings (RFC-0013, cited at `rfcs/RFC-0026-ui.md:34-35`). | `rfcs/RFC-0026-ui.md:34-35`, `rfcs/RFC-0026-ui.md:126-129`, `examples/domdemo.vyrn:17-21` |
| Stale closure in a watcher | IMPOSSIBLE BY CONSTRUCTION | Vyrn has no watchers and no stored closures. RFC-0023 refused stored closures; RFC-0026 states "no stored closures" (`rfcs/RFC-0026-ui.md:34`). Handlers are `export extern fn name(arg: String)` — name-dispatched by the host, not closures (`web/vyrn-dom.js:290-298`). The view is a pure function that reads module state by name on every call. There is no callback that can capture a stale value, because there is no callback. | `rfcs/RFC-0026-ui.md:34`, `web/vyrn-dom.js:290-298`, `examples/domdemo.vyrn:100-111` |
| Reactive object leaking between server requests | POSSIBLE | Vyrn's server is a singleton wasm module. Module-level `let mut` on the server persists across requests, because module state is root-only and initialised once at boot (RFC-0013). A developer who writes `let mut cache: Map<String, Data> = []` at module scope on the server creates a singleton shared by every request. The page system does not encourage this: `load()` runs per request and passes data through the view function, and there is no `useState` equivalent on the server. But the language does not prevent it. The Vue/Nuxt-specific mechanisms (effect scope retention, reactive singletons) are absent because Vyrn has no reactivity system. The underlying hazard — module-level mutable state shared across requests — is the same. | `rfcs/RFC-0026-ui.md:34-35` (module state is root-only, RFC-0013), `std/ui.vyrn:408-455` (per-request `load()` + payload, no shared reactive state) |
| Hydration mismatch from a non-deterministic render | IMPOSSIBLE BY CONSTRUCTION | Vyrn never hydrates. The server renders `toHtmlString(view())`. The client builds DOM from `toJson(view())` fresh. There is no server-DOM-to-client-vdom comparison step. RFC-0069 states "no hydration circus" (`rfcs/RFC-0069-universal-pages.md:22-24`). The language rule: the server and client share the same `view()` function (`rfcs/RFC-0026-ui.md:59-61`), and the client starts rendering only at the first navigation, never re-rendering the SSR'd first page. A non-deterministic page could produce different SSR output vs. client-rendered output on a soft nav, but that is content divergence between two separate renders, not a hydration mismatch. No comparison step means no mismatch. | `rfcs/RFC-0069-universal-pages.md:22-24`, `rfcs/RFC-0026-ui.md:59-61`, `web/vyrn-dom.js:691-708` (initial mount builds from scratch, no hydration) |
| Infinite update loop | IMPOSSIBLE BY CONSTRUCTION | The host owns the loop (RFC-0013). `rerender()` is called once after each handler returns (`web/vyrn-dom.js:297`). The view function is pure: it reads state and returns an `Html` tree. It does not mutate state and it is not an extern handler. There is no reactive effect that re-triggers itself. A handler cannot call itself, because handlers are dispatched by DOM events, not by reactive writes. Subscriptions are data diffed by value after each render, not reactive callbacks that re-enter the loop. The language rule: RFC-0013 (host owns the loop) and RFC-0026 (view is a pure function, handlers are name-dispatched externs). | `web/vyrn-dom.js:290-298`, `rfcs/RFC-0026-ui.md:34-35`, `examples/domdemo.vyrn:100-111` |
| Memory leak from an effect never stopped | IMPOSSIBLE BY CONSTRUCTION | Vyrn has no effects and no watchers. Subscriptions are data: a `Sub` enum (`Every`, `Keydown`) declared by `vyrnSubs()` and diffed by value after each render (`std/html.vyrn:84-86`, `web/vyrn-dom.js:616-636`). When a subscription disappears from the list, the host unwires it. There are no effect scopes to retain. RFC-0023 refused stored closures, so no retained callbacks exist. The `destroy()` method tears down all subscriptions and effects (`web/vyrn-dom.js:720-734`). The one escape hatch — `data-effect` for imperative DOM — is reconciled by DOM presence: when the DOM node disappears, the cleanup runs (`web/vyrn-dom.js:558-563`). A missing cleanup function is a user error in an explicit escape hatch, not a framework-level effect leak. | `std/html.vyrn:84-86`, `web/vyrn-dom.js:616-636`, `web/vyrn-dom.js:720-734`, `rfcs/RFC-0026-ui.md:34` (no stored closures) |

### What Vyrn pays for that

A model that cannot have stale closures or lost tracking recomputes more. Every
event calls `view()` in full, which rebuilds the entire `Html` tree. The diff
then absorbs the changes. For a small view this cost is negligible. For a large
view — thousands of nodes — every keystroke rebuilds the whole tree in wasm.

The patch protocol (RFC-0035) reduces the wire cost: `vyrnPatch()` diffs in wasm
and ships only `PatchOp` changes across the extern boundary, so the boundary
cost is O(changes) not O(tree). But the `view()` call itself is still O(tree).

The deferred M5 compiled reactivity is the planned answer: the template compiler
would statically know which bindings read which store fields, and compile a
`patch(dirty)` that re-evaluates only affected bindings — Svelte-style targeted
patches, no closures, no proxies (`rfcs/RFC-0026-ui.md:379-385`). This is not
shipped.

The cost of the full re-render model against a dependency-tracked model is NOT
MEASURED. There is no existing `bench` block for the view/diff cycle in the
repository, and this job is read-only except for the output file. To measure it,
a bench block would need to call `view()` and `diff(old, new)` over a tree of
representative size and count the median time per event. The `examples/` corpus
has no such block today; `examples/htmltree.vyrn` builds a tree but carries no
`bench` block.

---

## Open questions for the owner

These are the choices this census surfaced. Each is marked
RECOMMENDATION, NOT A DECISION.

1. **Should Vyrn ship M5 compiled reactivity?** The full re-render model is
   correct and simple. Its cost is O(tree) per event in wasm, hidden by the diff
   for small views and by the patch protocol for the wire. The M5 design
   (`rfcs/RFC-0026-ui.md:379-385`) would make it O(changes) in compute too, with
   no runtime signals. The trade-off: a more complex template compiler, and a
   second code path in the host runtime. RECOMMENDATION, NOT A DECISION.

2. **Should the server isolate module state per request?** Vyrn's server is a
   singleton wasm module. Module-level `let mut` persists across requests. The
   page system does not use it for request data, but nothing prevents a
   developer from creating a shared mutable binding. A per-request module
   instance (or a checked rule against server-side module-level `let mut`) would
   close the gap. The trade-off: re-instantiating the module per request costs
   boot time, and a checker rule may be too strict for legitimate caches.
   RECOMMENDATION, NOT A DECISION.

3. **Is the `data-effect` missing-cleanup failure acceptable?** The imperative
   escape hatch (`web/vyrn-dom.js:567-591`) runs a registered effect when a
   `data-effect` node appears and calls its cleanup when the node disappears. A
   registration that returns no cleanup function leaves nothing to run on
   removal. The host could warn when an effect registration returns no cleanup.
   The trade-off: some effects are genuinely one-shot and need no cleanup, so a
   warning would be a false positive for those. RECOMMENDATION, NOT A DECISION.

4. **Should a non-deterministic page render produce a diagnostic?** A page that
   reads `timeNow()` or a random source can produce different output on the
   server vs. a client-rendered soft navigation. Vyrn does not hydrate, so this
   is not a mismatch — it is content divergence between two separate renders.
   The owner may consider this acceptable (the soft-nav model re-renders from
   fresh data anyway) or may want a lint against non-deterministic calls in
   page view functions. RECOMMENDATION, NOT A DECISION.
