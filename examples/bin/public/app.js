// Client boot for the pastebin create island (RFC-0026 M2 + RFC-0019 + RFC-0067
// + RFC-0035 patch loop). One wasm module renders the create form into `#app` on
// the home page. The RPC transport supplies `vyrnRpcCall`; vyrn-dom owns the
// instance and the render loop.
//
// After every RPC completion we re-render, then poll `takeNav()`: on a successful
// create the client stashes a `/p/<id>` target, and we hand it to
// `window.vyrnNav.navigate` for a SOFT navigation (falling back to a hard
// location change if vyrn-nav is absent).
//
// RFC-0067 island shape: `bootApp` INSTANTIATES the wasm module exactly once and
// returns an instance whose `mount(el)` re-attaches the view to the `#app` a soft
// nav brings back. The wasm instance — and the draft it holds in module state —
// therefore survives navigating away and back; only the DOM view is re-mounted.
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
    // `takeNav()` and `vyrnRenderPage()` return Strings; declare them so the
    // export glue DECODES the wasm pointer into a JS string (RFC-0012 String ABI
    // asymmetry — a raw export call otherwise hands back the pointer integer).
    exportReturns: { takeNav: "string", vyrnRenderPage: "string" },
  });

  // RFC-0069 §3: hand the universal-page renderer to vyrn-nav so a soft nav renders
  // the next page client-side from its JSON payload. The wasm instance survives
  // navigations (RFC-0067), so once registered the renderer stays live.
  if (window.vyrnNav && typeof window.vyrnNav.setPageRenderer === "function" && typeof app.exports.vyrnRenderPage === "function") {
    window.vyrnNav.setPageRenderer((payloadJson) => app.exports.vyrnRenderPage(payloadJson));
  }

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
    // Re-attach the SAME wasm instance's view to the `#app` node a soft nav
    // brought back (RFC-0067). Instantiation and the RPC binding above happened
    // once; here we only repaint — the draft in module state is intact.
    mount(el) {
      app.remount(el);
    },
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

// With vyrn-nav present, hand it the island so it owns boot / re-mount across soft
// navigations; without it, boot directly.
if (window.vyrnNav && typeof window.vyrnNav.registerIsland === "function") {
  window.vyrnNav.registerIsland("#app", bootOrReport);
} else {
  const el = document.getElementById("app");
  if (el) bootOrReport(el);
}
