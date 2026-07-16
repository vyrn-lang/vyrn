// Client boot for shelf (RFC-0026 M2 + RFC-0019). One wasm module serves both
// the home list (mounts #app, bootList) and the detail card (mounts #book,
// bootDetail with the id from data-book-id). The RPC transport supplies
// `vyrnRpcCall`; vyrn-dom owns the instance and the render loop.
import { mount } from "/vyrn-runtime/vyrn-dom.js";
import { makeRpcTransport } from "/vyrn-runtime/vyrn-rpc.js";

async function main() {
  const home = document.getElementById("app");
  const detail = document.getElementById("book");
  const mountEl = home || detail;
  if (!mountEl) return;

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

  if (detail) {
    const id = detail.getAttribute("data-book-id") || "0";
    app.exports.bootDetail(id);
  } else {
    app.exports.bootList("");

    // A genuine SERVER 422: a typed stub can never build an invalid AddBookReq,
    // so this posts a raw bad body straight through the transport. The reply
    // (422 with the server's own Issues) flows to onAddBook -> the issues panel.
    const bad = document.createElement("button");
    bad.textContent = "force server 422 (raw wire: empty title, bad url)";
    bad.className = "raw422";
    bad.onclick = () =>
      rpc.vyrnRpcCall("addBook", JSON.stringify({ title: "", url: "nope", tags: ["waaaaaaaaaaaaaaaaaaaaaaaaay-too-long"] }));
    mountEl.parentNode.appendChild(bad);
  }
  app.rerender();
}

main().catch((e) => {
  const box = document.getElementById("app") || document.getElementById("book");
  if (box) box.innerHTML = `<p style="color:#c0392b">boot error: ${e && e.message ? e.message : e}</p>`;
});
