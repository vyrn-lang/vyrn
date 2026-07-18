// The client boot for the fullstack home page (RFC-0026 M2). Extracted verbatim
// from the former public/index.html so the HTML surface can be server-rendered by
// a std/ui page while the interactive RPC demo (client.wasm + vyrn-dom + the RPC
// transport) is unchanged.
import { mount } from "/vyrn-runtime/vyrn-dom.js";
import { makeRpcTransport } from "/vyrn-runtime/vyrn-rpc.js";

const $ = (id) => document.getElementById(id);
const status = $("status");

async function main() {
  const bytes = await (await fetch("/client.wasm")).arrayBuffer();

  // The RPC transport supplies the `vyrnRpcCall` extern; vyrn-dom owns the
  // instance, the DOM, and the render loop.
  const rpc = makeRpcTransport({ baseUrl: "" });
  const app = await mount(bytes, $("app"), {
    extern: { vyrnRpcCall: rpc.vyrnRpcCall },
  });

  // Bind the transport to the module's dispatchers, wrapped so that every
  // completion (which runs the callback the stub stored, updating module state)
  // triggers a re-render — the reply is async, so there is no event to piggyback on.
  rpc.bind(
    new Proxy(app.exports, {
      get(target, prop) {
        const v = target[prop];
        if (
          typeof prop === "string" &&
          prop.startsWith("vyrnRpcDone") &&
          typeof v === "function"
        ) {
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

  status.innerHTML = `<span class="ok">mounted</span> · client.wasm exports: ${Object.keys(app.exports).join(", ")}`;

  // Raw create with an invalid age — the one payload a typed stub could never
  // build, so no stub (and no pending callback) is involved: POST it raw, then
  // hand the server's 422 body to the exported renderer (RFC-0040 §2).
  $("badBtn").onclick = async () => {
    const res = await fetch("/rpc/createUser", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ name: "Bob", age: 200 }),
    });
    app.exports.showServerIssues(await res.text());
    app.rerender();
  };

  // The procedure registry — a raw GET, straight from the server.
  $("schemaBtn").onclick = async () => {
    const text = await (await fetch("/rpc/$schema")).text();
    try {
      $("schemaOut").textContent = JSON.stringify(JSON.parse(text), null, 2);
    } catch {
      $("schemaOut").textContent = text;
    }
  };
}

main().catch((e) => {
  status.innerHTML = `<span style="color:#c0392b">boot error: ${e && e.message ? e.message : e}</span>`;
});
