// The ASCII hero (design brief section 6). Vyrn computes, JavaScript paints.
//
// Each frame the host calls `heroFrame(cols, rows, t, charAr)` on a live wasm
// instance built from `examples/herofield.vyrn`, and gets back one glyph per
// cell. This file measures the grid, maps each glyph to an alpha, and calls
// `fillText`. One boundary crossing per frame, not one per cell.
//
// The field itself — the noise, the gate, the lattice — is Vyrn source, and the
// parity harness proves the interpreter, the native binary and wasm agree on its
// floating point to the last bit.
//
// If the module will not load, the same formula is evaluated here instead. That
// is the brief's circuit breaker, not a second implementation to maintain: the
// hero must always paint something, and a blank hero looks broken.
import { runVyrn } from "/wasi-min.js";

const RAMP = ".:+=VYRN#$&"; // eleven glyphs, dark to light; VYRN in the mid-tones

// ---------------------------------------------------------------------------
// The fallback field, transcribed from examples/herofield.vyrn. Only reached
// when the wasm module is unavailable.
// ---------------------------------------------------------------------------

const unit = (x) => (x < 0 ? 0 : x > 1 ? 1 : x);

function fieldAt(u, v, t) {
  const noise =
    Math.sin(9.7 * u + 5.3 * v + 1.7 * t) *
    Math.sin(6.1 * u - 8.3 * v - 1.1 * t) *
    Math.sin(13.7 * (u + v) + 0.7 * t);
  const lattice = Math.sin(7 * v) * Math.cos(3 * u - 1.6 * t);
  const w = unit((u + 0.05) / 0.25);
  const d = u + 0.05;
  const seam = 1 / (1 + 900 * d * d);
  return (1 - w) * 0.85 * noise + w * lattice + 1.3 * seam;
}

function fallbackFrame(cols, rows, t, charAR) {
  let out = "";
  for (let r = 0; r < rows; r++) {
    const v = (2 * (r / rows) - 1) / charAR;
    for (let c = 0; c < cols; c++) {
      const u = 2 * (c / cols) - 1;
      const lum = Math.floor(255 * unit((fieldAt(u, v, t) + 1.1) / 2.6));
      out += lum === 0 ? " " : RAMP[(lum * RAMP.length - 1) >> 8];
    }
    out += "\n";
  }
  return out;
}

// ---------------------------------------------------------------------------

export function mountHero(canvas, opts = {}) {
  const O = Object.assign({ cellPx: 7, lineFactor: 8.08, min: 0.1, max: 1 }, opts);
  const ctx = canvas.getContext("2d", { alpha: true });
  const reduced = matchMedia("(prefers-reduced-motion: reduce)").matches;
  const coarse = matchMedia("(pointer: coarse)").matches;
  const interval = /^((?!chrome|android).)*safari/i.test(navigator.userAgent) ? 100 : 50;

  let dpr = 1, cols = 0, rows = 0, cellW = 0, cellH = 0, charAR = 0.6;
  let last = 0, raf = 0, running = false, faults = [];
  const t0 = performance.now();
  let vyrnFrame = null; // set once the module instantiates

  // Brightness rides on alpha, so the fill strings are built once per theme
  // rather than a hundred thousand times a second.
  const ink = () => {
    const rgb = getComputedStyle(canvas).getPropertyValue("--field").trim() || "18,50,88";
    return RAMP.split("").map((_, i) => {
      const a = O.min + ((O.max - O.min) * i) / (RAMP.length - 1);
      return `rgba(${rgb},${a.toFixed(3)})`;
    });
  };
  let INK = ink();

  function measure() {
    // The stylesheet owns the canvas's CSS size (`width: 100%`, a fixed height),
    // and this only sets the backing store, which does not affect layout. An
    // earlier draft pinned the CSS size here in pixels; that pin froze the grid
    // at whatever width the page happened to have on the very first frame, which
    // on a slow stylesheet is the wrong one and never corrects itself.
    const box = canvas.getBoundingClientRect();
    const w = Math.max(1, Math.round(box.width));
    const h = Math.max(1, Math.round(box.height));
    dpr = Math.max(1, Math.floor(devicePixelRatio || 1)); // integer only, or the grid blurs
    const bw = w * dpr, bh = h * dpr;
    if (canvas.width !== bw || canvas.height !== bh) {
      canvas.width = bw;
      canvas.height = bh;
    }
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.imageSmoothingEnabled = false;
    ctx.textBaseline = "top";

    // Measure the character aspect ratio. Assuming 0.6 shears the picture.
    ctx.font = "100px ui-monospace, SFMono-Regular, Menlo, monospace";
    const m = ctx.measureText("M");
    const hgt = (m.actualBoundingBoxAscent || 70) + (m.actualBoundingBoxDescent || 30);
    charAR = Math.min(2, Math.max(0.3, m.width / hgt));
    ctx.font = `${O.cellPx * 1.55}px ui-monospace, SFMono-Regular, Menlo, monospace`;

    cols = Math.max(20, Math.min(150, Math.floor(w / O.cellPx)));
    rows = Math.max(12, Math.min(70, Math.floor(h / ((O.cellPx * O.lineFactor) / 7))));
    cellW = w / cols;
    cellH = h / rows;
    INK = ink();
  }

  function paint(t) {
    const w = canvas.width / dpr, h = canvas.height / dpr;
    ctx.clearRect(0, 0, w, h);
    const text = vyrnFrame
      ? vyrnFrame(BigInt(cols), BigInt(rows), t, charAR)
      : fallbackFrame(cols, rows, t, charAR);
    let r = 0, c = 0;
    for (let i = 0; i < text.length; i++) {
      const ch = text[i];
      if (ch === "\n") { r++; c = 0; continue; }
      if (ch !== " ") {
        // A cell the field left dark draws nothing: the paper stays clean.
        const at = RAMP.indexOf(ch);
        ctx.fillStyle = INK[at < 0 ? 0 : at];
        ctx.fillText(ch, c * cellW, r * cellH);
      }
      c++;
    }
  }

  // Three exceptions inside three seconds and the canvas gives up for good.
  function guarded(fn) {
    try {
      fn();
    } catch (err) {
      const now = performance.now();
      faults = faults.filter((f) => now - f < 3000);
      faults.push(now);
      if (faults.length >= 3) {
        stop();
        const pre = document.createElement("pre");
        pre.className = "hero-static";
        pre.setAttribute("aria-hidden", "true");
        pre.textContent = "vyrn";
        canvas.replaceWith(pre);
      }
    }
  }

  function loop(now) {
    raf = requestAnimationFrame(loop);
    if (now - last < interval) return; // manual gate, never setInterval
    last = now;
    guarded(() => paint((now - t0) / 1000));
  }

  function start() {
    if (running || reduced) return;
    running = true;
    raf = requestAnimationFrame(loop);
  }
  function stop() {
    running = false;
    cancelAnimationFrame(raf);
  }

  guarded(() => { measure(); paint(0.6); }); // one frame, always

  // Observe the canvas itself. Its CSS size comes from the stylesheet, so its
  // box reports every change that matters — including the one a late stylesheet
  // causes after the first frame is already painted.
  const remeasure = () =>
    guarded(() => { measure(); paint(reduced ? 0.6 : (performance.now() - t0) / 1000); });
  new ResizeObserver(remeasure).observe(canvas);
  addEventListener("resize", remeasure, { passive: true });
  new IntersectionObserver((es) => (es[0].isIntersecting ? start() : stop()), {
    threshold: 0.1,
  }).observe(canvas);
  document.addEventListener("visibilitychange", () => (document.hidden ? stop() : start()));
  matchMedia("(prefers-color-scheme: dark)").addEventListener("change", remeasure);

  // Swap the fallback for the real module as soon as it lands. Until then the
  // hero is already painting, so nothing waits on the network.
  loadModule().then((fn) => {
    if (!fn) return;
    vyrnFrame = fn;
    canvas.dataset.source = "wasm"; // the page can be checked from outside
    remeasure();
  });

  return { start, stop };
}

/// Instantiate examples/herofield.vyrn and hand back its exported `heroFrame`,
/// or null if anything at all goes wrong.
async function loadModule() {
  try {
    const res = await fetch("/hero.wasm");
    if (!res.ok) return null;
    const bytes = new Uint8Array(await res.arrayBuffer());
    // `main` prints a sample frame and the field's bit patterns. That is the
    // parity proof, and in a page it belongs in the console, not on the page.
    const run = await runVyrn(bytes, { onStdout: () => {}, onStderr: () => {} });
    const fn = run.exports && run.exports.heroFrame;
    return typeof fn === "function" ? fn : null;
  } catch (err) {
    return null;
  }
}
