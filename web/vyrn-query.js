// vyrn-query.js — a tiny query cache over vyrn RPC (RFC-0019), the "colada".
//
// Zero dependencies, ~120 lines. It owns cache policy so the transport does not:
// requests are keyed by (proc, requestJson); an in-flight request is deduped
// (concurrent callers share one fetch); a settled entry is served stale within
// `staleTime`; `invalidate` drops entries and `refetch` forces a new fetch.
//
// Shape (RFC-0040 §2): the cache IS a transport — it supplies the module's
// `vyrnRpcCall` extern, so IT mints the request ids the module's pending-callback
// maps key on, and every settle (network OR cache) dispatches the matching
// `vyrnRpcDone<Proc>(id, status, body)` with the SAME id the extern returned.
// The stub stored the caller's callback under that id, so the callback runs with
// a decoded `Validation<T>` exactly as if the wire had answered. Drop it in place
// of vyrn-rpc.js's transport (same extern slot), not in front of it.
//
// A cache hit dispatches on a microtask, never synchronously: the module stores
// the pending callback only after `vyrnRpcCall` returns the id, so a synchronous
// dispatch would fire before the callback exists.
//
// Honest scope: no retries, no background refetch, no window-focus revalidation,
// no garbage collection of old keys (call `invalidate` yourself). That is on
// purpose — this is the smallest cache that makes the dedupe + invalidate story
// real, not a TanStack Query clone.
//
// Usage:
//   import { createQueryClient } from "./vyrn-query.js";
//   import { runVyrn } from "./wasi-min.js";
//   const qc = createQueryClient({ baseUrl: "", staleTime: 5000 });
//   const res = await runVyrn(bytes, { extern: { vyrnRpcCall: qc.vyrnRpcCall } });
//   qc.bind(res.exports);   // let the cache reach the dispatchers
//   // now call an exported stub; a fresh cached reply skips the network:
//   exports.getUser(/* the module builds+sends the request */);
//   qc.invalidate("getUser");  // drop all getUser entries
//   qc.fetchCount;             // observable network hits

/** `getUser` → `vyrnRpcDoneGetUser` — the locked dispatcher-naming convention. */
function dispatcherName(proc) {
  return "vyrnRpcDone" + proc.charAt(0).toUpperCase() + proc.slice(1);
}

export function createQueryClient({ baseUrl = "", staleTime = 0, fetchImpl } = {}) {
  const doFetch = fetchImpl || ((...a) => fetch(...a));
  const cache = new Map(); // key -> { settled, status, body, ts, waiters: [id] }
  let exportsRef = null;
  let nextId = 1;
  let fetchCount = 0;

  const keyOf = (proc, reqJson) => proc + "\n" + reqJson;

  // Feed a (status, body) into the module's dispatcher for `proc` under request
  // id `id`, so the pending callback the stub stored under that id runs.
  function dispatch(proc, id, status, body) {
    const name = dispatcherName(proc);
    if (exportsRef && typeof exportsRef[name] === "function") {
      exportsRef[name](BigInt(id), BigInt(status), body);
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
   * The `vyrnRpcCall` extern: mint an id, then serve the (proc, body) request
   * from cache policy —
   * - a fresh settled entry within `staleTime` ⇒ re-dispatch cached on a
   *   microtask, no fetch;
   * - an in-flight entry ⇒ join its waiters, no second fetch (dedupe);
   * - otherwise ⇒ one network fetch; on settle, dispatch to every waiter id.
   */
  function vyrnRpcCall(proc, reqJson) {
    const id = nextId++;
    const key = keyOf(proc, reqJson);
    const entry = cache.get(key);
    const now = Date.now();

    if (entry && entry.settled && now - entry.ts < staleTime) {
      Promise.resolve().then(() => dispatch(proc, id, entry.status, entry.body));
      return BigInt(id);
    }
    if (entry && !entry.settled) {
      entry.waiters.push(id); // in-flight: dedupe
      return BigInt(id);
    }

    const rec = { settled: false, status: 0, body: "", ts: now, waiters: [id] };
    cache.set(key, rec);
    networkFetch(proc, reqJson).then((r) => {
      rec.settled = true;
      rec.status = r.status;
      rec.body = r.body;
      rec.ts = Date.now();
      const waiters = rec.waiters;
      rec.waiters = [];
      for (const w of waiters) {
        dispatch(proc, w, r.status, r.body);
      }
    });
    return BigInt(id);
  }

  /** Drop cache entries: an exact `proc\nreqJson` key, or every entry for a proc. */
  function invalidate(target) {
    for (const key of [...cache.keys()]) {
      if (key === target || key.startsWith(target + "\n")) {
        cache.delete(key);
      }
    }
  }

  return {
    vyrnRpcCall,
    bind(exports) {
      exportsRef = exports;
    },
    invalidate,
    cache,
    get fetchCount() {
      return fetchCount;
    },
  };
}
