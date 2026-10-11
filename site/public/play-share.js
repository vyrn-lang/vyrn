export function encodeSource(src) {
  const bytes = new TextEncoder().encode(src);
  let binary = "";
  // In chunks: fromCharCode.apply takes one argument per byte and overflows on a long program.
  for (let i = 0; i < bytes.length; i += 0x8000) {
    binary += String.fromCharCode.apply(null, bytes.subarray(i, i + 0x8000));
  }
  return btoa(binary).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

function decodeSource(code) {
  let b64 = code.replace(/-/g, "+").replace(/_/g, "/");
  b64 += "=".repeat((4 - (b64.length % 4)) % 4);
  const binary = atob(b64);
  return new TextDecoder().decode(Uint8Array.from(binary, (c) => c.charCodeAt(0)));
}

export function sourceFromHash() {
  const m = /[#&]c=([A-Za-z0-9\-_]+)/.exec(location.hash || "");
  if (!m) return null;
  try {
    return decodeSource(m[1]);
  } catch (err) {
    return null;
  }
}

export function projectFromHash() {
  if (!location.hash.startsWith("#p=")) return null;
  try {
    const payload = JSON.parse(decodeSource(location.hash.slice(3)));
    return payload && Array.isArray(payload.files) ? payload : null;
  } catch (err) {
    return null;
  }
}
