// vyrn-query.js — a tiny query cache over vyrn RPC (RFC-0019), the "colada".
//
// Zero dependencies, ~110 lines. It owns cache policy so the transport does not:
// requests are keyed by (proc, requestJson); an in-flight request is deduped
// (concurrent callers share one fetch); a settled entry is served stale within
// `staleTime`; `invalidate` drops entries and `refetch` forces a new fetch.
//
// It drives the SAME per-procedure dispatchers as vyrn-rpc.js — on every settle
// (network OR cache) it calls the module's `vyrnRpcDone<Proc>(id, status, body)`,
// so the user's Vyrn `on<Proc>` handler runs with a decoded `Validation<T>`,
// exactly as if the wire had answered. It is therefore a drop-in in front of the
// transport, not a parallel path.
//
// Honest scope: no ret[ries], no background refetch, no window-focus revalidation,
// no garbage collection of old keys (call `invalidate` yourself). That is on
// purpose — this is the smallest cache that makes the demo's dedupe + invalidate
// story real, not a TanStack Query clone.
//
// Usage:
//   import { createQueryClient } from "./vyrn-query.js";
//   const qc = createQueryClient({ exports, baseUrl: "" });
//   qc.query("getUser", '{"id":7}', { staleTime: 5000 });  // -> Promise<{status, body}>
//   qc.invalidate("getUser");                                // drop all getUser entries
//   qc.fetchCount;                                           // observable network hits

/** `getUser` → `vyrnRpcDoneGetUser` — the locked dispatcher-naming convention. */
function dispatcherName(proc) {
  return "vyrnRpcDone" + proc.charAt(0).toUpperCase() + proc.slice(1);
}

export function createQueryClient({ exports, baseUrl = "", fetchImpl } = {}) {
  const doFetch = fetchImpl || ((...a) => fetch(...a));
  const cache = new Map(); // key -> { promise, settled, status, body, ts }
  let nextId = 1;
  let fetchCount = 0;

  const keyOf = (proc, reqJson) => proc + "\n" + reqJson;

  // Feed a (status, body) into the module's dispatcher for `proc`, so the Vyrn
  // handler updates whether the data came from the network or the cache.
  function dispatch(proc, status, body) {
    const name = dispatcherName(proc);
    if (exports && typeof exports[name] === "function") {
      exports[name](BigInt(nextId++), BigInt(status), body);
    }
  }

  function networkFetch(proc, reqJson) {
    fetchCount++;
    return doFetch(baseUrl + "/rpc/" + proc, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: reqJson,
    }).then(
      (res) => res.text().then((text) => ({ status: res.status, body: text })),
      () => ({ status: 0, body: "" }) // network failure → unreachable
    );
  }

  /**
   * Run (or reuse) a query. Returns a Promise of `{ status, body }`.
   * - a fresh settled entry within `staleTime` ⇒ re-dispatch cached, no fetch;
   * - an in-flight entry ⇒ share its promise, no second fetch (dedupe);
   * - otherwise ⇒ one network fetch, cached and dispatched on settle.
   */
  function query(proc, reqJson, { staleTime = 0 } = {}) {
    const key = keyOf(proc, reqJson);
    const entry = cache.get(key);
    const now = Date.now();

    if (entry && entry.settled && now - entry.ts < staleTime) {
      dispatch(proc, entry.status, entry.body);
      return entry.promise;
    }
    if (entry && !entry.settled) {
      return entry.promise; // in-flight: dedupe
    }

    const rec = { settled: false, status: 0, body: "", ts: now, promise: null };
    rec.promise = networkFetch(proc, reqJson).then((r) => {
      rec.settled = true;
      rec.status = r.status;
      rec.body = r.body;
      rec.ts = Date.now();
      dispatch(proc, r.status, r.body);
      return r;
    });
    cache.set(key, rec);
    return rec.promise;
  }

  /** Drop cache entries: an exact `proc\nreqJson` key, or every entry for a proc. */
  function invalidate(target) {
    for (const key of [...cache.keys()]) {
      if (key === target || key.startsWith(target + "\n")) {
        cache.delete(key);
      }
    }
  }

  /** Force a fresh fetch for one (proc, reqJson), ignoring any cached entry. */
  function refetch(proc, reqJson, opts) {
    invalidate(keyOf(proc, reqJson));
    return query(proc, reqJson, opts);
  }

  return {
    query,
    invalidate,
    refetch,
    cache,
    get fetchCount() {
      return fetchCount;
    },
  };
}
