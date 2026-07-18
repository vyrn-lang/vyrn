// Client boot for the pastebin create island (RFC-0026 M2 + RFC-0019 + RFC-0034
// + RFC-0035 patch loop). One wasm module renders the create form into `#app` on
// the home page. The RPC transport supplies `vyrnRpcCall`; vyrn-dom owns the
// instance and the render loop.
//
// After every RPC completion we re-render, then poll `takeNav()`: on a successful
// create the client stashes a `/p/<id>` target, and we hand it to
// `window.vyrnNav.navigate` for a SOFT navigation (falling back to a hard
// location change if vyrn-nav is absent).
import { mount } from "/vyrn-runtime/vyrn-dom.js";
import { makeRpcTransport } from "/vyrn-runtime/vyrn-rpc.js";

function softNav(url) {
  if (window.vyrnNav && typeof window.vyrnNav.navigate === "function") {
    window.vyrnNav.navigate(url);
  } else {
    window.location.assign(url);
  }
}

async function bootApp(mountEl) {
  const wasmBytes = await (await fetch("/client.wasm")).arrayBuffer();
  const rpc = makeRpcTransport({ baseUrl: "" });
  const app = await mount(wasmBytes, mountEl, {
    extern: { vyrnRpcCall: rpc.vyrnRpcCall },
    // `takeNav()` returns a String; declare it so the export glue DECODES the
    // wasm pointer into a JS string (RFC-0012 String ABI asymmetry — a raw
    // export call otherwise hands back the pointer integer). See NOTES.
    exportReturns: { takeNav: "string" },
  });

  // Drain any pending soft-nav target the client set during a handler.
  function drainNav() {
    const target = app.exports.takeNav ? app.exports.takeNav() : "";
    if (target) softNav(target);
  }

  // Every RPC completion runs the callback the stub stored (updating module
  // state); re-render, then check for a queued navigation.
  rpc.bind(
    new Proxy(app.exports, {
      get(target, prop) {
        const v = target[prop];
        if (typeof prop === "string" && prop.startsWith("vyrnRpcDone") && typeof v === "function") {
          return (...args) => {
            const r = v(...args);
            app.rerender();
            drainNav();
            return r;
          };
        }
        return v;
      },
    })
  );

  app.rerender();

  return {
    destroy() {
      app.destroy();
    },
  };
}

function bootOrReport(mountEl) {
  return bootApp(mountEl).catch((e) => {
    if (mountEl) mountEl.innerHTML = `<p style="color:#c0392b">boot error: ${e && e.message ? e.message : e}</p>`;
  });
}

// With vyrn-nav present, hand it the island so it owns boot / re-boot across soft
// navigations; without it, boot directly.
if (window.vyrnNav && typeof window.vyrnNav.registerIsland === "function") {
  window.vyrnNav.registerIsland("#app", bootOrReport);
} else {
  const el = document.getElementById("app");
  if (el) bootOrReport(el);
}
