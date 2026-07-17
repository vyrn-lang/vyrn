// Client boot for shelf (RFC-0026 M2 + RFC-0019 + RFC-0034). One wasm module
// serves both the home list (mounts #app, bootList) and the detail card (mounts
// #book, bootDetail with the id from data-book-id). The RPC transport supplies
// `vyrnRpcCall`; vyrn-dom owns the instance and the render loop.
//
// Under soft navigation (RFC-0034) the browser never re-runs this <script> on a
// nav, so the boot registers itself as an ISLAND with vyrn-nav instead of
// booting inline: vyrn-nav boots it on first appearance and re-boots it fresh
// after every navigation that lands on a page mounting `#app`. With vyrn-nav
// absent (progressive enhancement), this boots directly, exactly as before.
import { mount } from "/vyrn-runtime/vyrn-dom.js";
import { makeRpcTransport } from "/vyrn-runtime/vyrn-rpc.js";

// Boot the client app into `mountEl`. Returns an instance with `destroy()` so
// vyrn-nav can tear it down before a re-boot (fresh state = MPA semantics).
async function bootApp(mountEl) {
  const home = mountEl.id === "app";

  const wasmBytes = await (await fetch("/client.wasm")).arrayBuffer();
  const rpc = makeRpcTransport({ baseUrl: "" });
  const app = await mount(wasmBytes, mountEl, {
    extern: { vyrnRpcCall: rpc.vyrnRpcCall },
  });

  // Every RPC completion updates module state via on<Proc>; re-render after each.
  rpc.bind(
    new Proxy(app.exports, {
      get(target, prop) {
        const v = target[prop];
        if (typeof prop === "string" && prop.startsWith("vyrnRpcDone") && typeof v === "function") {
          return (...args) => {
            const r = v(...args);
            app.rerender();
            return r;
          };
        }
        return v;
      },
    })
  );

  let badBtn = null;
  if (home) {
    app.exports.bootList("");

    // A genuine SERVER 422: a typed stub can never build an invalid AddBookReq,
    // so this posts a raw bad body straight through the transport. The reply
    // (422 with the server's own Issues) flows to onAddBook -> the issues panel.
    badBtn = document.createElement("button");
    badBtn.textContent = "force server 422 (raw wire: empty title, bad url)";
    badBtn.className = "raw422";
    badBtn.onclick = () =>
      rpc.vyrnRpcCall("addBook", JSON.stringify({ title: "", url: "nope", tags: ["waaaaaaaaaaaaaaaaaaaaaaaaay-too-long"] }));
    mountEl.parentNode.appendChild(badBtn);
  } else {
    const id = mountEl.getAttribute("data-book-id") || "0";
    app.exports.bootDetail(id);
  }
  app.rerender();

  return {
    destroy() {
      app.destroy();
      if (badBtn) badBtn.remove();
    },
  };
}

function bootOrReport(mountEl) {
  return bootApp(mountEl).catch((e) => {
    if (mountEl) mountEl.innerHTML = `<p style="color:#c0392b">boot error: ${e && e.message ? e.message : e}</p>`;
  });
}

// With vyrn-nav present, hand it the island so it owns boot / re-boot across
// soft navigations; it boots immediately if `#app` is already on the page.
// Without it, boot directly (works for both the home and detail mounts).
if (window.vyrnNav && typeof window.vyrnNav.registerIsland === "function") {
  window.vyrnNav.registerIsland("#app", bootOrReport);
} else {
  const el = document.getElementById("app") || document.getElementById("book");
  if (el) bootOrReport(el);
}
