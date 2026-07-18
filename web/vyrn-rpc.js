// vyrn-rpc.js — the browser transport for typed RPC (RFC-0019).
//
// A wasm module built with std/rpc's `rpcClient` generator imports one shared
// extern, `vyrn.vyrnRpcCall(name, body) -> Int64`, and exports one completion
// dispatcher per procedure, `vyrnRpcDone<Proc>(id, status, body)`. This module
// supplies that extern (a `fetch` POST to `<baseUrl>/rpc/<name>`) and, when the
// request settles, calls the matching dispatcher back into the module with the
// SAME id the extern returned — so the module routes the reply to the pending
// callback the stub stored under that id (RFC-0040 §2). The extern ABI is
// unchanged from the pre-callback design, so this transport is untouched by it.
//
// The proc→dispatcher name is the shared convention: `vyrnRpcDone` + the
// procedure name with its first letter uppercased (`getUser` → `vyrnRpcDoneGetUser`).
// vyrn-rpc made the request, so it always knows which dispatcher owns the reply,
// and it echoes back the request id the extern minted, so the module's pending
// map finds the right callback (RFC-0019 + RFC-0040 §2).
//
// Usage:
//   import { runVyrnRpc } from "./vyrn-rpc.js";
//   const { exports } = await runVyrnRpc(bytes, { baseUrl: "" });
//   // call an exported stub with a callback; its reply flows to that callback:
//   exports.getUser(/* the module builds+sends the request */);
//
// Or wire it onto an existing runVyrn call yourself:
//   import { makeRpcTransport } from "./vyrn-rpc.js";
//   const rpc = makeRpcTransport({ baseUrl: "" });
//   const res = await runVyrn(bytes, { extern: { vyrnRpcCall: rpc.vyrnRpcCall } });
//   rpc.bind(res.exports);   // let the transport reach the dispatchers

import { runVyrn } from "./wasi-min.js";

/** `getUser` → `vyrnRpcDoneGetUser` — the locked dispatcher-naming convention. */
export function dispatcherName(proc) {
  return "vyrnRpcDone" + proc.charAt(0).toUpperCase() + proc.slice(1);
}

/**
 * Build the RPC transport: the `vyrnRpcCall` extern plus a `bind(exports)` to
 * hand it the module's exported dispatchers (available only after instantiate).
 * `baseUrl` defaults to same-origin; `fetchImpl` is overridable for tests.
 */
export function makeRpcTransport({ baseUrl = "", fetchImpl } = {}) {
  const doFetch = fetchImpl || ((...a) => fetch(...a));
  let exportsRef = null;
  let nextId = 1;

  function complete(proc, id, status, body) {
    const name = dispatcherName(proc);
    if (!exportsRef || typeof exportsRef[name] !== "function") {
      // A stub fired but its dispatcher is missing — the module was built
      // without this procedure, or `bind` was never called.
      console.warn(`vyrn-rpc: no dispatcher \`${name}\` on the module for \`${proc}\``);
      return;
    }
    // The export ABI (wasi-min.js) takes i64 args as BigInt and a String arg as
    // a JS string (it allocates+copies inside the module).
    exportsRef[name](BigInt(id), BigInt(status), body);
  }

  // The single shared extern. wasi-min.js decodes the two String params to JS
  // strings and converts our BigInt return to the i64 request id.
  function vyrnRpcCall(name, body) {
    const id = nextId++;
    doFetch(baseUrl + "/rpc/" + name, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: body,
    })
      .then((res) => res.text().then((text) => complete(name, id, res.status, text)))
      // A network failure (offline, DNS, CORS) reports status 0 = "unreachable",
      // which the generated unifier turns into an `rpc.transport` Issue.
      .catch(() => complete(name, id, 0, ""));
    return BigInt(id);
  }

  return {
    vyrnRpcCall,
    bind(exports) {
      exportsRef = exports;
    },
  };
}

/**
 * Convenience: instantiate a client wasm module with the RPC transport already
 * wired. Extra `runVyrn` hooks (onStdout, exportReturns, more externs) pass
 * through; a caller-supplied `extern.vyrnRpcCall` is overridden by the transport.
 */
export async function runVyrnRpc(wasmBytes, { baseUrl = "", fetchImpl, ...hooks } = {}) {
  const rpc = makeRpcTransport({ baseUrl, fetchImpl });
  const result = await runVyrn(wasmBytes, {
    ...hooks,
    extern: { ...(hooks.extern || {}), vyrnRpcCall: rpc.vyrnRpcCall },
  });
  rpc.bind(result.exports);
  return { ...result, rpc };
}
