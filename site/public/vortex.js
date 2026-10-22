// The landing page's whirlpool: one WebGL2 scene behind five panels, paged by wheel, touch and keys.
// Without WebGL2 the panels stay a plain stacked page; `vx-live` on <html> switches the CSS to paging.
// Units are scene units: the water spans radius 0.06 (the eye) to 6 (the rim), y = funnel(r).

const TAU = Math.PI * 2;
const FOV = (38 * Math.PI) / 180;
const L = (a, b, t) => a + (b - a) * t;
const SS = (x, a, b) => {
  const t = Math.min(1, Math.max(0, (x - a) / (b - a)));
  return t * t * (3 - 2 * t);
};
const funnel = (r) => -0.55 / (r + 0.16) + 0.55 / 6.2;

class V3 {
  constructor(x = 0, y = 0, z = 0) {
    this.x = x;
    this.y = y;
    this.z = z;
  }
  set(x, y, z) {
    this.x = x;
    this.y = y;
    this.z = z;
    return this;
  }
  copy(v) {
    return this.set(v.x, v.y, v.z);
  }
  clone() {
    return new V3(this.x, this.y, this.z);
  }
  add(v) {
    return this.set(this.x + v.x, this.y + v.y, this.z + v.z);
  }
  addScaled(v, s) {
    return this.set(this.x + v.x * s, this.y + v.y * s, this.z + v.z * s);
  }
  lerp(v, t) {
    return this.set(L(this.x, v.x, t), L(this.y, v.y, t), L(this.z, v.z, t));
  }
  setY(y) {
    this.y = y;
    return this;
  }
}
const surf = (r, a) => new V3(Math.cos(a) * r, funnel(r) + 0.03, Math.sin(a) * r);

/* Shaders. Every pass writes linear light; the last one tone-maps (ACES) and encodes sRGB. */

// The water turns at one rate on every panel: the slow drift of the last one.
const SWIRL = 0.11;

const FOG = "float fogK(float d){return exp(-pow(0.06*d,2.0));}";

const WATER_VS = `in vec4 seed;
uniform mat4 uV,uP;uniform float uFocus,uT,uRing,uHit,uPx,uSpin,uVis,uDrainT,uDrain,uCalm;
out vec3 vC;out float vA;
float F(float r){return -0.55/(r+0.16)+0.55/6.2;}
void main(){
 float spd=(0.018+0.03*seed.z)*mix(0.2,6.0,uDrain);
 float cyc=seed.x+uT*spd,life=fract(cyc),born=(floor(cyc)-seed.x)/spd;
 float r=mix(6.0,0.06,pow(life,0.55));
 float a=seed.y+uSpin*(uT*${SWIRL}/(r+0.25)+life*9.0/(r+0.6));
 float band=sin(a*3.0+log(r)*6.0);
 float y=F(r)+0.02*band*smoothstep(0.2,1.5,r)*(1.0-uCalm)+(seed.w-0.5)*0.015*r;
 vec4 mv=uV*vec4(cos(a)*r,y,sin(a)*r,1.0);gl_Position=uP*mv;
 float near=smoothstep(0.0,0.06,abs(r-uRing));
 vec3 water=mix(vec3(0.22,0.42,0.62),vec3(0.75,0.86,1.0),pow(seed.w,3.0));
 vC=mix(mix(vec3(1.0,0.95,0.85)*2.2,water,near),vec3(1.0,0.36,0.25)*1.6,uHit*(1.0-near)*step(r,uRing+0.06));
 float alive=mix(1.0,step(born,uDrainT),uDrain);
 float arms=mix(0.75,mix(0.12+0.88*smoothstep(0.2,1.0,band),0.75,uCalm),smoothstep(uRing+0.15,uRing+0.9,r));
 vA=uVis*alive*smoothstep(0.0,0.08,life)*smoothstep(0.06,0.5,r)*mix(arms,0.75,uCalm);
 float dz=-mv.z-uFocus,coc=clamp(-dz*0.25,0.0,1.6)+clamp(dz*0.05,0.0,0.5);
 vA/=1.0+coc*coc*1.2;
 gl_PointSize=uPx*(1.0+2.6*pow(seed.z,6.0))*(3.2/-mv.z)*(1.0+coc*1.6);}`;

// A droplet carries a value; at the ring it is proved (passes), checked (an input, amber) or refused (shatters, coral).
const DROPS_VS = `in vec4 dseed;in vec3 shard;
uniform mat4 uV,uP;uniform float uFocus,uT,uRing,uPx,uRule,uVis;
out vec3 vC;out float vA;
float F(float r){return -0.55/(r+0.16)+0.55/6.2;}
float R0(float l){return mix(6.0,0.06,pow(l,0.55));}
float ang(float l,float r){return dseed.y+uT*${SWIRL}/(r+0.25)+l*9.0/(r+0.6);}
void main(){
 float l=fract(dseed.x+uT*0.03),lc=pow(clamp((6.0-uRing)/5.94,0.0,1.0),1.0/0.55);
 float val=1.0+floor(dseed.w*40.0);bool inp=dseed.z<0.3;bool pass=val>=uRule;float k=l-lc;
 vec3 p;vec3 c;float a=1.0,sz=6.0;
 vec3 moon=vec3(0.95,0.97,1.0)*1.6,amber=vec3(1.0,0.62,0.25)*3.0,coral=vec3(1.0,0.34,0.22)*3.0;
 float lone=shard.x>0.0?0.0:1.0;
 if(k<0.0||pass){float r=R0(l);float an=ang(l,r);p=vec3(cos(an)*r,F(r)+0.01,sin(an)*r);float q=k*70.0;float fl=exp(-q*q);
  c=inp?mix(moon*0.8,amber,fl):moon;a=lone*smoothstep(0.0,0.06,l)*smoothstep(0.06,0.4,r);sz=inp?5.0+7.0*fl:6.0;}
 else{float an=ang(lc,uRing);vec3 rp=vec3(cos(an),0.0,sin(an));vec3 tg=vec3(-sin(an),0.0,cos(an));
  if(inp){float an2=an+k*4.0;p=vec3(cos(an2)*uRing,F(uRing)+0.01,sin(an2)*uRing);c=amber*0.9;a=lone*exp(-k*6.0);sz=5.0;}
  else{float t=k*2.2;p=rp*uRing+vec3(0.0,F(uRing),0.0)+(rp*shard.y+tg*shard.z)*t*0.9+vec3(0.0,t*1.6-t*t*9.0,0.0);c=coral*1.5;a=exp(-k*7.0);sz=4.5;}}
 vec4 mv=uV*vec4(p,1.0);gl_Position=uP*mv;
 float dz=-mv.z-uFocus,coc=clamp(-dz*0.25,0.0,1.6)+clamp(dz*0.05,0.0,0.5);
 vC=c;vA=a*uVis/(1.0+coc*coc*1.2);gl_PointSize=uPx*sz*(3.2/-mv.z)*(1.0+coc*1.6);}`;

const DOT_FS = (pow) => `in vec3 vC;in float vA;out vec4 o;
void main(){float k=1.0-smoothstep(0.0,0.5,length(gl_PointCoord-0.5));o=vec4(vC*vA*${pow},1.0);}`;

// A glow sprite: aP = position and world size, aC = colour and opacity.
const SPRITE_VS = `in vec4 aP;in vec4 aC;uniform mat4 uV,uP;uniform float uK;out vec3 vC;${FOG}
void main(){vec4 mv=uV*vec4(aP.xyz,1.0);gl_Position=uP*mv;gl_PointSize=aP.w*uK/-mv.z;vC=aC.rgb*aC.a*fogK(-mv.z);}`;
const SPRITE_FS = `in vec3 vC;out vec4 o;
void main(){float d=length(gl_PointCoord-0.5)*2.0;float a=d<0.25?mix(1.0,0.5,d/0.25):mix(0.5,0.0,clamp((d-0.25)/0.75,0.0,1.0));o=vec4(vC*a,1.0);}`;

// A thin torus in the xz plane: a = (cos u, sin u, cos v, sin v); the tube scales with the ring, as a scaled mesh does.
// Its triangles wind counter-clockwise seen from outside, so back faces are culled and each ring is drawn once.
const RING_VS = `in vec4 a;uniform mat4 uV,uP;uniform float uS,uTube;uniform vec3 uO;out float vF;${FOG}
void main(){float R=uS*(1.0+uTube*a.z);vec4 mv=uV*vec4(uO+vec3(a.x*R,-uS*uTube*a.w,a.y*R),1.0);gl_Position=uP*mv;vF=fogK(-mv.z);}`;
const RING_FS = `in float vF;uniform vec4 uC;out vec4 o;void main(){o=vec4(uC.rgb*vF,uC.a);}`;

const QUAD_VS = `out vec2 vUv;void main(){vec2 p=vec2(float((gl_VertexID<<1)&2),float(gl_VertexID&2));vUv=p;gl_Position=vec4(p*2.0-1.0,0.0,1.0);}`;
const BRIGHT_FS = `in vec2 vUv;uniform sampler2D t;out vec4 o;
void main(){vec4 c=texture(t,vUv);float v=dot(c.rgb,vec3(0.299,0.587,0.114));o=vec4(c.rgb*smoothstep(0.32,0.33,v),1.0);}`;
const BLUR_FS = `in vec2 vUv;uniform sampler2D t;uniform vec2 uStep;uniform int uK;out vec4 o;
float g(float i,float s){return 0.39894*exp(-0.5*i*i/(s*s))/s;}
void main(){float s=float(uK);float w0=g(0.0,s);vec3 sum=texture(t,vUv).rgb*w0;float ws=w0;
 for(int i=1;i<11;i++){if(i>=uK)break;float w=g(float(i),s);vec2 d=uStep*float(i);sum+=(texture(t,vUv+d).rgb+texture(t,vUv-d).rgb)*w;ws+=2.0*w;}
 o=vec4(sum/ws,1.0);}`;
const MERGE_FS = `in vec2 vUv;uniform sampler2D t0,t1,t2,t3,t4;out vec4 o;
void main(){o=vec4(0.75*(0.52*texture(t0,vUv).rgb+0.56*texture(t1,vUv).rgb+0.6*texture(t2,vUv).rgb+0.64*texture(t3,vUv).rgb+0.68*texture(t4,vUv).rgb),1.0);}`;
const GRADE_FS = `in vec2 vUv;uniform sampler2D tS,tB;uniform float uT;out vec4 o;
float h(vec2 p){return fract(sin(dot(p,vec2(12.9898,78.233))+uT)*43758.5453);}
vec3 at(vec2 uv){return texture(tS,uv).rgb+texture(tB,uv).rgb;}
vec3 rrt(vec3 v){vec3 a=v*(v+0.0245786)-0.000090537;vec3 b=v*(0.983729*v+0.4329510)+0.238081;return a/b;}
void main(){vec2 c=vUv-0.5;float d=dot(c,c);
 vec3 col=vec3(at(vUv+c*0.004).r,at(vUv).g,at(vUv-c*0.004).b);
 col*=1.0-smoothstep(0.12,0.55,d*1.4);col+=(h(vUv*999.0)-0.5)*0.035;
 col=max(col,0.0)*(1.1/0.6);
 col=mat3(0.59719,0.07600,0.02840,0.35458,0.90834,0.13383,0.04823,0.01566,0.83777)*col;
 col=clamp(mat3(1.60475,-0.10208,-0.00327,-0.53108,1.10813,-0.07276,-0.07367,-0.00605,1.07602)*rrt(col),0.0,1.0);
 o=vec4(mix(pow(col,vec3(1.0/2.4))*1.055-0.055,col*12.92,vec3(lessThanEqual(col,vec3(0.0031308)))),1.0);}`;

function compile(gl, vs, fs) {
  const p = gl.createProgram();
  for (const [type, src] of [[gl.VERTEX_SHADER, vs], [gl.FRAGMENT_SHADER, fs]]) {
    const s = gl.createShader(type);
    gl.shaderSource(s, "#version 300 es\nprecision highp float;\nprecision highp int;\n" + src);
    gl.compileShader(s);
    if (!gl.getShaderParameter(s, gl.COMPILE_STATUS)) throw new Error(gl.getShaderInfoLog(s));
    gl.attachShader(p, s);
  }
  gl.linkProgram(p);
  if (!gl.getProgramParameter(p, gl.LINK_STATUS)) throw new Error(gl.getProgramInfoLog(p));
  const u = {};
  for (let i = 0, n = gl.getProgramParameter(p, gl.ACTIVE_UNIFORMS); i < n; i++) {
    const name = gl.getActiveUniform(p, i).name;
    u[name] = gl.getUniformLocation(p, name);
  }
  return { p, u, a: (name) => gl.getAttribLocation(p, name) };
}

function buffer(gl, prog, name, data, size, usage = gl.STATIC_DRAW) {
  const b = gl.createBuffer();
  gl.bindBuffer(gl.ARRAY_BUFFER, b);
  gl.bufferData(gl.ARRAY_BUFFER, data, usage);
  const loc = prog.a(name);
  gl.enableVertexAttribArray(loc);
  gl.vertexAttribPointer(loc, size, gl.FLOAT, false, 0, 0);
  return b;
}

function perspective(aspect) {
  const f = 1 / Math.tan(FOV / 2), near = 0.05, far = 90, nf = 1 / (near - far);
  return new Float32Array([f / aspect, 0, 0, 0, 0, f, 0, 0, 0, 0, (far + near) * nf, -1, 0, 0, 2 * far * near * nf, 0]);
}

// The view matrix and the camera's axes; x, y and z are unit vectors, the camera looks down -z.
function lookAt(e, t) {
  let zx = e.x - t.x, zy = e.y - t.y, zz = e.z - t.z;
  let n = Math.hypot(zx, zy, zz);
  zx /= n; zy /= n; zz /= n;
  let xx = zz, xz = -zx;
  n = Math.hypot(xx, xz);
  xx /= n; xz /= n;
  const yx = zy * xz, yy = zz * xx - zx * xz, yz = -zy * xx;
  const m = new Float32Array([xx, yx, zx, 0, 0, yy, zy, 0, xz, yz, zz, 0,
    -(xx * e.x + xz * e.z), -(yx * e.x + yy * e.y + yz * e.z), -(zx * e.x + zy * e.y + zz * e.z), 1]);
  return { m, x: new V3(xx, 0, xz), y: new V3(yx, yy, yz), z: new V3(zx, zy, zz) };
}

function seeds(n, w) {
  const s = new Float32Array(n * w);
  for (let i = 0; i < n; i++) {
    s[i * 4] = Math.random();
    s[i * 4 + 1] = Math.random() * TAU;
    s[i * 4 + 2] = Math.random();
    s[i * 4 + 3] = Math.random();
  }
  return s;
}

// Mounts the scene on `root`. Returns false, and changes nothing, when WebGL2 is missing.
export function vortexWidget(root) {
  const canvas = document.createElement("canvas");
  const gl = canvas.getContext("webgl2", { antialias: false, alpha: false, depth: false, powerPreference: "high-performance" });
  if (!gl) return false;
  const half = gl.getExtension("EXT_color_buffer_float") ? [gl.RGBA16F, gl.HALF_FLOAT] : [gl.RGBA8, gl.UNSIGNED_BYTE];
  let P;
  try {
    P = {
      water: compile(gl, WATER_VS, DOT_FS("k")),
      drops: compile(gl, DROPS_VS, DOT_FS("k*k")),
      sprite: compile(gl, SPRITE_VS, SPRITE_FS),
      ring: compile(gl, RING_VS, RING_FS),
      bright: compile(gl, QUAD_VS, BRIGHT_FS),
      blur: compile(gl, QUAD_VS, BLUR_FS),
      merge: compile(gl, QUAD_VS, MERGE_FS),
      grade: compile(gl, QUAD_VS, GRADE_FS),
    };
  } catch (err) {
    console.error("the whirlpool did not compile", err);
    return false;
  }

  const life = new AbortController();
  const on = (target, kind, fn, opts = {}) => target.addEventListener(kind, fn, { ...opts, signal: life.signal });
  const $ = (sel) => root.querySelector(sel);
  const still = matchMedia("(prefers-reduced-motion: reduce)").matches;
  const PX = Math.min(devicePixelRatio, 1.5);
  canvas.className = "vx-canvas";
  canvas.setAttribute("aria-hidden", "true");
  root.prepend(canvas);
  const overlay = document.createElement("div");
  overlay.className = "vx-labels";
  overlay.setAttribute("aria-hidden", "true");
  root.append(overlay);
  document.documentElement.classList.add("vx-live");

  /* geometry */
  const vaos = {};
  const vao = (name, fill) => {
    vaos[name] = gl.createVertexArray();
    gl.bindVertexArray(vaos[name]);
    fill();
    gl.bindVertexArray(null);
  };
  const WATER_N = 90000;
  vao("water", () => buffer(gl, P.water, "seed", seeds(WATER_N, 4), 4));
  const D = 260, S = 7, DN = D * S;
  const dseed = new Float32Array(DN * 4), shard = new Float32Array(DN * 3);
  for (let i = 0; i < D; i++) {
    const a = Math.random(), b = Math.random() * TAU, k = Math.random(), v = Math.random();
    for (let j = 0; j < S; j++) {
      const o = i * S + j, th = Math.random() * TAU, ph = Math.random();
      dseed.set([a, b, k, v], o * 4);
      shard.set([j, Math.cos(th) * (0.4 + ph), Math.sin(th) * (0.4 + ph)], o * 3);
    }
  }
  vao("drops", () => {
    buffer(gl, P.drops, "dseed", dseed, 4);
    buffer(gl, P.drops, "shard", shard, 3);
  });
  const RU = 256, RV = 8, ringAttr = [], ringIdx = [];
  for (let j = 0; j <= RV; j++) {
    for (let i = 0; i <= RU; i++) {
      const u = (i / RU) * TAU, v = (j / RV) * TAU;
      ringAttr.push(Math.cos(u), Math.sin(u), Math.cos(v), Math.sin(v));
    }
  }
  for (let j = 1; j <= RV; j++) {
    for (let i = 1; i <= RU; i++) {
      const a = (RU + 1) * j + i - 1, b = (RU + 1) * (j - 1) + i - 1, c = b + 1, d = a + 1;
      ringIdx.push(a, b, d, b, c, d);
    }
  }
  vao("ring", () => {
    buffer(gl, P.ring, "a", new Float32Array(ringAttr), 4);
    const ib = gl.createBuffer();
    gl.bindBuffer(gl.ELEMENT_ARRAY_BUFFER, ib);
    gl.bufferData(gl.ELEMENT_ARRAY_BUFFER, new Uint16Array(ringIdx), gl.STATIC_DRAW);
  });
  const SPRITE_MAX = 256, spriteData = new Float32Array(SPRITE_MAX * 8);
  let spriteBuf;
  vao("sprite", () => {
    spriteBuf = gl.createBuffer();
    gl.bindBuffer(gl.ARRAY_BUFFER, spriteBuf);
    gl.bufferData(gl.ARRAY_BUFFER, spriteData.byteLength, gl.DYNAMIC_DRAW);
    for (const [name, off] of [["aP", 0], ["aC", 16]]) {
      const loc = P.sprite.a(name);
      gl.enableVertexAttribArray(loc);
      gl.vertexAttribPointer(loc, 4, gl.FLOAT, false, 32, off);
    }
  });
  vao("quad", () => {});

  /* scene objects: sprites and rings are plain records the frame packs into draws */
  const sprites = [];
  const sprite = (r, g, b, s) => {
    const sp = { p: new V3(), c: new V3(r, g, b), s, o: 1 };
    sprites.push(sp);
    return sp;
  };
  const rings = [];
  const ring = (tube) => {
    const rg = { s: 1, tube, p: new V3(), c: new V3(1, 1, 1), o: 0 };
    rings.push(rg);
    return rg;
  };

  /* render targets, rebuilt on resize */
  let W = 1, H = 1, scene, bright, mips = [], merged;
  const target = (w, h) => {
    const t = gl.createTexture();
    gl.bindTexture(gl.TEXTURE_2D, t);
    gl.texImage2D(gl.TEXTURE_2D, 0, half[0], w, h, 0, gl.RGBA, half[1], null);
    for (const [k, v] of [[gl.TEXTURE_MIN_FILTER, gl.LINEAR], [gl.TEXTURE_MAG_FILTER, gl.LINEAR], [gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE], [gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE]]) gl.texParameteri(gl.TEXTURE_2D, k, v);
    const f = gl.createFramebuffer();
    gl.bindFramebuffer(gl.FRAMEBUFFER, f);
    gl.framebufferTexture2D(gl.FRAMEBUFFER, gl.COLOR_ATTACHMENT0, gl.TEXTURE_2D, t, 0);
    return { t, f, w, h };
  };
  const free = (rt) => {
    if (!rt) return;
    gl.deleteTexture(rt.t);
    gl.deleteFramebuffer(rt.f);
  };
  let proj = perspective(1), aspect = 1;
  function resize() {
    W = Math.max(1, Math.round(innerWidth * PX));
    H = Math.max(1, Math.round(innerHeight * PX));
    canvas.width = W;
    canvas.height = H;
    aspect = innerWidth / innerHeight;
    proj = perspective(aspect);
    [scene, bright, merged, ...mips.flat()].forEach(free);
    scene = target(W, H);
    let w = Math.round(W / 2), h = Math.round(H / 2);
    bright = target(w, h);
    merged = target(w, h);
    mips = [];
    for (let i = 0; i < 5; i++) {
      mips.push([target(Math.max(1, w), Math.max(1, h)), target(Math.max(1, w), Math.max(1, h))]);
      w = Math.round(w / 2);
      h = Math.round(h / 2);
    }
  }
  resize();
  on(window, "resize", resize);

  /* camera */
  const cam = { p: new V3(0, 2, 6), look: new V3(), az: 0, view: lookAt(new V3(0, 2, 6), new V3()) };
  const project = (v) => {
    const m = cam.view.m, q = proj;
    const vx = m[0] * v.x + m[4] * v.y + m[8] * v.z + m[12];
    const vy = m[1] * v.x + m[5] * v.y + m[9] * v.z + m[13];
    const vz = m[2] * v.x + m[6] * v.y + m[10] * v.z + m[14];
    const cw = -vz, cx = q[0] * vx, cy = q[5] * vy, cz = q[10] * vz + q[14];
    return [((cx / cw + 1) / 2) * innerWidth, ((1 - cy / cw) / 2) * innerHeight, cz / cw];
  };
  // The ring's radius under the pointer, on the plane of the ring; -1 when the ray misses it.
  const radiusAt = (e, ringR) => {
    const t = Math.tan(FOV / 2), nx = (e.clientX / innerWidth) * 2 - 1, ny = 1 - (e.clientY / innerHeight) * 2;
    const v = cam.view, d = v.z.clone().set(-v.z.x, -v.z.y, -v.z.z).addScaled(v.x, nx * t * aspect).addScaled(v.y, ny * t);
    const k = (funnel(ringR) - cam.p.y) / d.y;
    if (!(k > 0)) return -1;
    return Math.hypot(cam.p.x + d.x * k, cam.p.z + d.z * k);
  };

  /* panels and paging */
  const panels = [...root.querySelectorAll("[data-panel]")];
  const PAGES = panels.length;
  const rail = document.createElement("nav");
  rail.className = "vx-rail";
  rail.setAttribute("aria-label", "Sections");
  rail.innerHTML = panels.map((p, i) => `<button type="button" data-i="${i}"><span>${p.dataset.panel}</span><i></i></button>`).join("");
  root.append(rail);
  let page = 0, pageP = 0, busy = 0;
  const TAGS = [];
  const go = (i) => {
    i = Math.max(0, Math.min(PAGES - 1, i));
    if (i === page) return;
    page = i;
    busy = performance.now();
    TAGS.forEach((t) => (t.t = Math.max(t.t, 1.6)));
  };
  rail.querySelectorAll("button").forEach((b) => on(b, "click", () => go(+b.dataset.i)));
  const inField = (e) => e.target.closest && e.target.closest("input, textarea, select, [contenteditable], [data-find]");
  const onControl = (e) => e.target.closest && e.target.closest("button, a, [role=switch]");
  on(window, "wheel", (e) => {
    if (e.target.closest && e.target.closest("[data-find]")) return;
    e.preventDefault();
    if (performance.now() - busy < 900 || Math.abs(e.deltaY) < 8) return;
    go(page + (e.deltaY > 0 ? 1 : -1));
  }, { passive: false });
  let ty = null;
  on(window, "touchstart", (e) => (ty = e.touches[0].clientY), { passive: true });
  on(window, "touchend", (e) => {
    if (ty === null) return;
    const d = ty - e.changedTouches[0].clientY;
    if (Math.abs(d) > 50) go(page + (d > 0 ? 1 : -1));
    ty = null;
  }, { passive: true });

  /* state the panels control */
  let rule = 18, hit = 0, ringR = 1.6, cur = 0, T = 0, spin = 1;
  const mouse = { x: 0, y: 0 }, sm = { x: 0, y: 0 };
  const ruleR = (v) => 0.5 + (v - 1) * 0.065;
  on(window, "keydown", (e) => {
    if (inField(e) || (e.key === " " && onControl(e))) return;
    if (e.key === "PageDown" || e.key === " ") {
      e.preventDefault();
      go(page + 1);
    } else if (e.key === "PageUp") {
      e.preventDefault();
      go(page - 1);
    } else if (cur === 0 && (e.key === "ArrowUp" || e.key === "ArrowDown")) {
      e.preventDefault();
      rule = Math.max(1, Math.min(40, rule + (e.key === "ArrowUp" ? 1 : -1)));
    }
  });
  const ops = { p: { sort: true, reverse: false, filter: false, map: false, push: false }, e: { gen: false, print: false, state: false } };
  let tgt = { spin: 1 };
  const text = (sel, t, bad) => {
    const el = $(sel);
    if (!el) return;
    el.textContent = t;
    if (bad !== undefined) el.classList.toggle("bad", bad);
  };
  function verdicts() {
    const o = ops.p;
    let f = o.sort ? "asc" : "none", drop = "";
    if (o.reverse && f !== "none") f = "desc";
    if (o.map && f !== "none") {
      drop = ".map(jitter) on line 6";
      f = "none";
    }
    if (o.push) {
      if (f !== "none") drop = "xs.push(17) on line 7";
      f = "none";
    }
    const ok = f === "asc";
    tgt = { spin: f === "desc" ? -1 : 1 };
    text("[data-vx=props-diag]", ok ? "proved: xs is sorted(byAge), so binarySearch may halve"
      : f === "desc" ? "refused: binarySearch needs xs sorted(byAge); .reverse() on line 4 made it sorted(byAge, descending)"
      : drop ? `refused: binarySearch needs xs sorted(byAge); ${drop} dropped the fact`
      : "refused: binarySearch needs xs sorted(byAge); nothing sorts xs", !ok);
    text("[data-vx=props-state]", ok ? "proved" : "refused", !ok);
    const e = ops.e, fx = [e.print && "io", e.state && "writes count"].filter(Boolean), pure = !(e.gen && fx.length);
    text("[data-vx=fx-state]", !pure ? "refused" : fx.length ? "effects: " + fx.join(", ") : "pure", !pure);
    text("[data-vx=fx-diag]", !pure ? `refused: a gen fn that reaches an effect (${fx.join(", ")}): a build must be deterministic`
      : fx.length ? `table() ${fx.join(" and ")}; a caller sees it in the signature's effects`
      : "table() is pure: same answer on every machine", !pure);
  }
  root.querySelectorAll("[data-op]").forEach((el) => {
    const g = ops[el.dataset.g];
    el.classList.toggle("on", g[el.dataset.op]);
    el.setAttribute("role", "switch");
    el.setAttribute("tabindex", "0");
    const flip = () => {
      g[el.dataset.op] = !g[el.dataset.op];
      el.classList.toggle("on", g[el.dataset.op]);
      el.setAttribute("aria-checked", String(g[el.dataset.op]));
      verdicts();
    };
    el.setAttribute("aria-checked", String(g[el.dataset.op]));
    on(el, "click", flip);
    on(el, "keydown", (e) => {
      if (e.key === "Enter" || e.key === " ") {
        e.preventDefault();
        e.stopPropagation();
        flip();
      }
    });
  });
  verdicts();
  let drainT = 1e9, draining = 0;
  const runBtn = $("[data-vx=run]"), refillBtn = $("[data-vx=refill]");
  if (runBtn) on(runBtn, "click", () => {
    drainT = T;
    draining = 1;
    text("[data-vx=own-state]", "running");
  });
  if (refillBtn) on(refillBtn, "click", () => {
    drainT = 1e9;
    draining = 0;
    text("[data-vx=freed]", "0");
    text("[data-vx=own-state]", "ready");
  });
  let drag = false;
  on(canvas, "pointerdown", (e) => {
    if (cur !== 0) return;
    const r = radiusAt(e, ringR);
    if (r > 0 && Math.abs(r - ringR) < 0.18) {
      drag = true;
      canvas.style.cursor = "grabbing";
    }
  });
  on(window, "pointerup", () => {
    drag = false;
    canvas.style.cursor = "";
  });
  on(window, "pointermove", (e) => {
    if (drag) {
      const r = radiusAt(e, ringR);
      if (r > 0) rule = Math.max(1, Math.min(40, Math.round((r - 0.5) / 0.065 + 1)));
      return;
    }
    mouse.x = e.clientX / innerWidth - 0.5;
    mouse.y = e.clientY / innerHeight - 0.5;
  });

  /* labels */
  const label = (cls, style) => {
    const el = document.createElement("div");
    el.className = cls;
    if (style) el.style.cssText = style;
    overlay.append(el);
    return el;
  };
  const place = (el, v, show) => {
    const [x, y, z] = project(v);
    el.style.left = x + "px";
    el.style.top = y + "px";
    el.style.opacity = z < 1 ? show : 0;
  };
  const tagAt = (v, t, col) => {
    TAGS.forEach((g) => (g.t = Math.max(g.t, 1.6)));
    const el = label("vx-tag");
    el.textContent = t;
    el.style.color = col;
    TAGS.push({ el, pos: v.clone(), t: 0 });
  };
  const lit = (sel) => {
    const e = $(sel);
    if (!e) return;
    e.classList.add("lit");
    clearTimeout(e._t);
    e._t = setTimeout(() => e.classList.remove("lit"), 900);
  };

  /* the hero: a ring is the rule; three tracked droplets name what the title says */
  const heroRing = ring(0.0035);
  const gates = [0, 1, 2].map(() => sprite(4, 2.4, 1, 0.09));
  const flash = sprite(5, 1.5, 1, 0.5);
  let slow = 0;
  const ruleLab = label("vx-rule");
  const LAB = ["p", "c", "x"].map((kind, i) => ({ life: ((3 - i) / 3) * 0.999, a0: i * 2.1, val: 0, kind, state: 0 }));
  // Aims the droplet so it reaches the ring at the front of the pool, where its tag can be read.
  function reval(d) {
    const r = Math.floor(Math.random() * 5);
    d.val = d.kind === "p" ? rule + 4 + r * 3 : d.kind === "x" ? Math.max(0, rule - 3 - r * 2) : rule + (Math.random() < 0.6 ? 2 + r : -2 - r);
    d.state = 0;
    const lc = Math.pow(Math.max(0, (6 - ringR) / 5.94), 1 / 0.55), Tc = T + (lc - d.life) / 0.2, goal = 2.07 + { p: -0.75, c: 0, x: 0.75 }[d.kind];
    d.a0 = goal - (Tc * 0.55) / (ringR + 0.25) - (lc * 9) / (ringR + 0.6);
  }
  LAB.forEach(reval);
  function heroFrame(dt, show, tshow) {
    place(ruleLab, new V3(-ringR * Math.cos(0.5), funnel(ringR), -ringR * Math.sin(0.5)), show);
    ruleLab.innerHTML = `value &gt;= ${rule}<small>drag the ring</small>`;
    for (const d of LAB) {
      d.life += dt * 0.2;
      if (d.life >= 1) {
        d.life = 0;
        reval(d);
      }
      const r = L(6, 0.06, Math.pow(d.life, 0.55)), a = d.a0 + (T * SWIRL) / (r + 0.25) + (d.life * 9) / (r + 0.6);
      if (d.state !== 0 || r > ringR || show <= 0.5) continue;
      const pass = d.val >= rule, at = new V3(Math.cos(a) * ringR, funnel(ringR), Math.sin(a) * ringR);
      if (d.kind === "c") {
        d.state = 1;
        lit("[data-vx=checks]");
        tagAt(at, `input ${d.val}: checked, ${pass ? "Some" : "None"}`, "var(--vx-amber)");
      } else if (pass) {
        d.state = 1;
        lit("[data-vx=proves]");
        tagAt(at, `${d.val}: proved`, "var(--vx-moon)");
      } else {
        d.state = 2;
        lit("[data-vx=refuses]");
        tagAt(at, `refused: ${d.val} is not an Age`, "var(--vx-coral)");
        slow = 1;
        flash.p.copy(at);
        flash.o = 1;
      }
    }
    for (let i = TAGS.length - 1; i >= 0; i--) {
      const t = TAGS[i];
      t.t += dt;
      const [x, y] = project(t.pos);
      t.el.style.left = x + "px";
      t.el.style.top = y - t.t * 18 + "px";
      t.el.style.opacity = tshow * (t.t < 0.2 ? t.t / 0.2 : Math.max(0, 1 - (t.t - 1.4) / 0.6));
      if (t.t > 2) {
        t.el.remove();
        TAGS.splice(i, 1);
      }
    }
  }

  /* properties: the hero's ring becomes a stack, one ring per line; one droplet carries the fact from rim to eye */
  const OPS = ["sort", "reverse", "filter", "map", "push"];
  const OPNAME = { sort: ".sort(byAge)", reverse: ".reverse()", filter: ".filter(isAdult)", map: ".map(jitter)", push: "xs.push(17)", bs: "binarySearch(xs, 30)" };
  const stack = [0, 1, 2, 3, 4, 5].map(() => ({ g: ring(0.0035), el: label("vx-pill"), r: 1, vis: 0, name: "", col: new V3(1, 1, 1) }));
  const drop = sprite(2.4, 2.5, 2.7, 0.16), halo = sprite(0.3, 2, 1.7, 0.5);
  const sparks = [...Array(14)].map(() => ({ s: sprite(4, 1.4, 1, 0.07), v: new V3(), t: 9 }));
  for (const sp of [drop, halo, ...sparks.map((k) => k.s), flash]) sp.o = 0;
  const RIM = 5;
  const fresh = (a) => ({ r: RIM, a, fact: "none", passed: -1, dead: false });
  let dl = fresh(1.2);
  const haloC = new V3();
  const burst = (at, col) => sparks.forEach((k) => {
    k.s.p.copy(at);
    k.v.set((Math.random() - 0.5) * 1.6, Math.random() * 1.2 + 0.3, (Math.random() - 0.5) * 1.6);
    k.t = 0;
    k.s.c.set(...col);
  });
  function stackFrame(dt, show) {
    const lines = [...OPS.filter((o) => ops.p[o]), "bs"], n = lines.length, back = -Math.PI / 2 - cam.az;
    stack.forEach((g, i) => {
      const live = i < n;
      if (live) {
        g.r = L(2.7, 0.85, n > 1 ? i / (n - 1) : 0);
        g.name = lines[i];
      }
      g.vis += ((live ? 1 : 0) - g.vis) * Math.min(1, dt * 5);
      g.g.s = g.r;
      g.g.p.set(0, funnel(g.r), 0);
      g.g.o = show * g.vis * (g.name === "bs" ? 1 : 0.7);
      g.g.c.lerp(g.col, Math.min(1, dt * 4));
      place(g.el, new V3(g.r * Math.cos(back), funnel(g.r), g.r * Math.sin(back)), show * g.vis * 0.9);
      g.el.textContent = OPNAME[g.name] || "";
    });
    if (show < 0.05) {
      drop.o = halo.o = 0;
      return;
    }
    dl.r -= dt * (0.5 + 1 / (dl.r + 0.3));
    dl.a += dt * (0.5 + 1.4 / (dl.r + 0.2));
    if (dl.r < 0.05) {
      dl = fresh(dl.a);
      stack.forEach((g) => g.col.set(1, 1, 1));
    }
    const r = dl.r, a = dl.a, at = new V3(Math.cos(a) * r, funnel(r) + 0.03, Math.sin(a) * r);
    const vis = show * Math.min(1, (RIM - r) / 0.5) * Math.min(1, r / 0.5), sink = Math.min(1, 0.25 + r / 0.8);
    for (let i = dl.passed + 1; i < n && !dl.dead; i++) {
      const g = stack[i];
      if (r > g.r) break;
      dl.passed = i;
      const op = g.name, ringAt = new V3(Math.cos(a) * g.r, funnel(g.r), Math.sin(a) * g.r);
      if (op === "sort") {
        dl.fact = "asc";
        g.col.set(0.4, 1.4, 1.3);
        tagAt(ringAt, "sort: now sorted(byAge)", "var(--vx-teal)");
      } else if (op === "reverse" && dl.fact !== "none") {
        dl.fact = dl.fact === "asc" ? "desc" : "asc";
        g.col.set(1.2, 0.9, 1.8);
        tagAt(ringAt, "reverse: sorted(byAge, descending)", "var(--vx-violet)");
      } else if (op === "filter" && dl.fact !== "none") {
        g.col.set(0.4, 1.4, 1.3);
        tagAt(ringAt, "filter: order kept", "var(--vx-teal)");
      } else if ((op === "map" || op === "push") && dl.fact !== "none") {
        dl.fact = "none";
        g.col.set(1.6, 0.55, 0.45);
        burst(ringAt, [4, 1.3, 1]);
        tagAt(ringAt, `${op}: the fact is dropped`, "var(--vx-coral)");
      } else if (op === "bs") {
        if (dl.fact === "asc") {
          g.col.set(1.6, 1.7, 1.9);
          lit("[data-vx=proves]");
          tagAt(ringAt, "binarySearch: proved", "var(--vx-moon)");
        } else {
          dl.dead = true;
          g.col.set(1.8, 0.5, 0.4);
          burst(ringAt, [4, 1.3, 1]);
          slow = 1;
          flash.p.copy(ringAt);
          flash.o = 1;
          tagAt(ringAt, "refused: needs sorted(byAge)", "var(--vx-coral)");
        }
      }
    }
    drop.p.copy(at);
    halo.p.copy(at);
    drop.o = vis * (dl.dead ? 0.45 : 1);
    drop.s = 0.16 * sink;
    if (dl.dead) drop.c.set(3, 1, 0.8);
    else drop.c.set(2.4, 2.5, 2.7);
    const want = dl.fact === "asc" ? new V3(0.3, 2, 1.7) : dl.fact === "desc" ? new V3(1.3, 0.9, 2.4) : new V3();
    haloC.lerp(want, Math.min(1, dt * 6));
    halo.c.copy(haloC);
    halo.o = vis * (dl.dead ? 0 : 1);
    halo.s = (0.42 + 0.06 * Math.sin(T * 4)) * sink;
    sparks.forEach((k) => {
      k.t += dt;
      k.v.y -= dt * 2.2;
      k.s.p.addScaled(k.v, dt);
      k.s.o = show * Math.max(0, 1 - k.t);
    });
  }

  /* effects: a seal round the pool; print is a jet over it, a write to count is a wake to a buoy */
  const SEAL = 2.35, seal = ring(0.003);
  seal.s = SEAL;
  seal.p.set(0, funnel(SEAL), 0);
  const outLab = label("vx-rule"), cntLab = label("vx-rule"), sealLab = label("vx-rule vx-dim");
  outLab.textContent = "stdout";
  cntLab.textContent = "count = 0";
  sealLab.textContent = "closed: nothing leaves the pool";
  const aJ = 4.75, aW = 5.85, J0 = surf(1.35, aJ), J1 = surf(3.05, aJ - 0.04), W0 = surf(1.3, aW), B = surf(2.85, aW + 0.04);
  const eOut = (t) => 1 - Math.pow(1 - t, 3);
  const ripples = [...Array(6)].map(() => ({ g: ring(0.004), t: 9, c: [1, 1, 1], r1: 0.25 }));
  const ripple = (at, col, r1 = 0.22) => {
    const k = ripples.reduce((a, b) => (a.t > b.t ? a : b));
    k.t = 0;
    k.g.p.copy(at);
    k.c = col;
    k.r1 = r1;
  };
  // One path per effect: out of the eye, along the surface, to its destination; `stop` cuts it at the seal.
  const RISE = 0.4, DUR = 0.8, acc = (t) => Math.pow(t, 1.5);
  const fromEye = (u, r0, a0) => {
    const r = L(0.07, r0, u), a = a0 - (1 - u) * 5.2;
    return new V3(Math.cos(a) * r, funnel(r) + 0.03, Math.sin(a) * r);
  };
  const jetPos = (u) => {
    const p = J0.clone().lerp(J1, u);
    p.y += Math.sin(u * Math.PI) * 0.3;
    return p;
  };
  const wakePos = (u) => {
    const p = W0.clone().lerp(B, u);
    p.y = funnel(Math.hypot(p.x, p.z)) + 0.03;
    return p;
  };
  const paths = {
    j: (q, stop) => (q < RISE ? fromEye(q / RISE, 1.35, aJ) : jetPos(Math.min(stop, (q - RISE) / (1 - RISE)))),
    w: (q, stop) => (q < RISE ? fromEye(q / RISE, 1.3, aW) : wakePos(Math.min(stop, (q - RISE) / (1 - RISE)))),
  };
  const mkBolt = (kind) => ({
    kind,
    ss: [...Array(12)].map((_, i) => {
      const sp = kind === "j" ? sprite(1.4, 1.4, 1.35, 0.05 - i * 0.0025) : sprite(0.3, 1.4, 1.25, 0.045 - i * 0.0022);
      sp.o = 0;
      return sp;
    }),
    t: 0, live: false, done: false, blocked: false,
  });
  const bolts = [];
  for (let i = 0; i < 3; i++) bolts.push(mkBolt("j"), mkBolt("w"));
  const buoy = sprite(0.6, 2.6, 2.2, 0.085);
  let jClock = 0, wClock = 0.5, count = 0, sealHit = 0;
  function effectsFrame(dt, show) {
    const { gen, print: io, state: st } = ops.e;
    sealHit = Math.max(0, sealHit - dt * 1.2);
    seal.o = show * (0.28 + 0.45 * sealHit);
    seal.c.set(L(0.85, 1.5, sealHit), L(0.9, 0.55, sealHit), L(1, 0.45, sealHit));
    place(sealLab, surf(SEAL, 4.2), show * (!io && !st ? 0.8 : 0));
    const fire = (k) => {
      const b = bolts.find((x) => x.kind === k && !x.live);
      if (b) Object.assign(b, { live: true, t: 0, done: false, blocked: gen });
    };
    jClock += dt;
    if (io && jClock > 0.95) {
      jClock = 0;
      fire("j");
    }
    wClock += dt;
    if (st && wClock > 1.1) {
      wClock = 0;
      fire("w");
    }
    for (const b of bolts) {
      if (!b.live) {
        b.ss.forEach((s) => (s.o = 0));
        continue;
      }
      b.t += dt;
      const stop = b.blocked ? (b.kind === "j" ? 0.5 : 0.55) : 1, path = paths[b.kind];
      b.ss.forEach((s, i) => {
        const q = acc(Math.max(0, b.t - i * 0.018) / DUR);
        s.p.copy(path(Math.min(1, q), stop));
        const fade = b.t > DUR ? Math.max(0, 1 - (b.t - DUR) * 6) : 1;
        s.o = show * (1 - i / b.ss.length) * fade * (b.kind === "j" ? 0.9 : 0.8);
      });
      if (!b.done && acc(b.t / DUR) >= 1) {
        b.done = true;
        if (b.blocked) {
          ripple(path(1, stop).setY(funnel(SEAL)), [1.6, 0.55, 0.45], 0.28);
          sealHit = 1;
        } else if (b.kind === "j") ripple(J1, [1, 1, 1], 0.2);
        else {
          count++;
          cntLab.textContent = "count = " + count;
          ripple(B, [0.4, 1.3, 1.15], 0.18);
        }
      }
      if (b.t > DUR + 0.35) b.live = false;
    }
    place(outLab, J1, show * (io && !gen ? 0.75 : 0));
    buoy.p.copy(B).add(new V3(0, 0.05 + Math.sin(T * 1.4) * 0.015, 0));
    buoy.o = show * (st ? 0.7 : 0.12);
    place(cntLab, B.clone().add(new V3(0, 0.1, 0)), show * (st ? 0.85 : 0));
    for (const k of ripples) {
      k.t += dt;
      const u = Math.min(1, k.t / 0.8);
      k.g.s = L(0.05, k.r1, eOut(u));
      k.g.o = show * (1 - u) * 0.7;
      k.g.c.set(...k.c);
    }
  }

  /* draw */
  const bind = (rt) => {
    gl.bindFramebuffer(gl.FRAMEBUFFER, rt ? rt.f : null);
    gl.viewport(0, 0, rt ? rt.w : W, rt ? rt.h : H);
  };
  const tex = (prog, name, unit, rt) => {
    gl.activeTexture(gl.TEXTURE0 + unit);
    gl.bindTexture(gl.TEXTURE_2D, rt.t);
    gl.uniform1i(prog.u[name], unit);
  };
  const quad = () => {
    gl.bindVertexArray(vaos.quad);
    gl.drawArrays(gl.TRIANGLES, 0, 3);
  };
  const KERNEL = [3, 5, 7, 9, 11];
  function render(U) {
    const V = cam.view.m;
    bind(scene);
    gl.clearColor(0, 0, 0, 1);
    gl.clear(gl.COLOR_BUFFER_BIT);
    gl.enable(gl.BLEND);
    gl.blendFunc(gl.ONE, gl.ONE);
    const camera = (prog) => {
      gl.useProgram(prog.p);
      gl.uniformMatrix4fv(prog.u.uV, false, V);
      gl.uniformMatrix4fv(prog.u.uP, false, proj);
    };
    camera(P.water);
    for (const [k, v] of Object.entries(U.water)) gl.uniform1f(P.water.u[k], v);
    gl.bindVertexArray(vaos.water);
    gl.drawArrays(gl.POINTS, 0, WATER_N);
    if (U.drops.uVis > 0.001) {
      camera(P.drops);
      for (const [k, v] of Object.entries(U.drops)) gl.uniform1f(P.drops.u[k], v);
      gl.bindVertexArray(vaos.drops);
      gl.drawArrays(gl.POINTS, 0, DN);
    }
    camera(P.ring);
    gl.blendFuncSeparate(gl.SRC_ALPHA, gl.ONE_MINUS_SRC_ALPHA, gl.ONE, gl.ONE_MINUS_SRC_ALPHA);
    gl.bindVertexArray(vaos.ring);
    gl.enable(gl.CULL_FACE);
    for (const rg of rings) {
      if (rg.o < 0.002) continue;
      gl.uniform1f(P.ring.u.uS, rg.s);
      gl.uniform1f(P.ring.u.uTube, rg.tube);
      gl.uniform3f(P.ring.u.uO, rg.p.x, rg.p.y, rg.p.z);
      gl.uniform4f(P.ring.u.uC, rg.c.x, rg.c.y, rg.c.z, Math.min(1, rg.o));
      gl.drawElements(gl.TRIANGLES, ringIdx.length, gl.UNSIGNED_SHORT, 0);
    }
    gl.disable(gl.CULL_FACE);
    let n = 0;
    for (const sp of sprites) {
      if (sp.o < 0.002 || n >= SPRITE_MAX) continue;
      spriteData.set([sp.p.x, sp.p.y, sp.p.z, sp.s, sp.c.x, sp.c.y, sp.c.z, Math.min(1, sp.o)], n * 8);
      n++;
    }
    if (n) {
      camera(P.sprite);
      gl.uniform1f(P.sprite.u.uK, H / (2 * Math.tan(FOV / 2)));
      gl.blendFunc(gl.ONE, gl.ONE);
      gl.bindVertexArray(vaos.sprite);
      gl.bindBuffer(gl.ARRAY_BUFFER, spriteBuf);
      gl.bufferSubData(gl.ARRAY_BUFFER, 0, spriteData, 0, n * 8);
      gl.drawArrays(gl.POINTS, 0, n);
    }
    gl.disable(gl.BLEND);
    // Bloom: a bright pass, five blurred mips, one weighted sum at half size.
    bind(bright);
    gl.useProgram(P.bright.p);
    tex(P.bright, "t", 0, scene);
    quad();
    let src = bright;
    gl.useProgram(P.blur.p);
    mips.forEach(([h, v], i) => {
      gl.uniform1i(P.blur.u.uK, KERNEL[i]);
      bind(h);
      tex(P.blur, "t", 0, src);
      gl.uniform2f(P.blur.u.uStep, 1 / src.w, 0);
      quad();
      bind(v);
      tex(P.blur, "t", 0, h);
      gl.uniform2f(P.blur.u.uStep, 0, 1 / h.h);
      quad();
      src = v;
    });
    bind(merged);
    gl.useProgram(P.merge.p);
    mips.forEach(([, v], i) => tex(P.merge, "t" + i, i, v));
    quad();
    bind(null);
    gl.useProgram(P.grade.p);
    tex(P.grade, "tS", 0, scene);
    tex(P.grade, "tB", 1, merged);
    gl.uniform1f(P.grade.u.uT, T % 10);
    quad();
  }

  /* frame */
  const wide = () => aspect > 1.2;
  let intro = 0, simT = 0, last = performance.now(), raf = 0;
  function frame(now) {
    const rdt = Math.min(0.05, (now - last) / 1000);
    last = now;
    slow = Math.max(0, slow - rdt * 1.4);
    const dt = rdt * (1 - 0.7 * Math.min(1, slow * 1.5));
    simT += dt;
    T = still ? 20 : simT;
    flash.o *= 0.93;
    flash.s = 0.25 + (1 - flash.o) * 0.9;
    intro = Math.min(1, intro + dt / 3.2);
    const ease = 1 - Math.pow(1 - intro, 3);
    sm.x = L(sm.x, mouse.x, Math.min(1, dt * 2));
    sm.y = L(sm.y, mouse.y, Math.min(1, dt * 2));
    pageP += (page - pageP) * Math.min(1, rdt * 5.5);
    if (Math.abs(page - pageP) < 0.002) pageP = page;
    cur = Math.round(pageP);
    const w = (i) => Math.max(0, 1 - Math.abs(pageP - i) * 1.7);
    panels.forEach((el, i) => {
      el.style.opacity = String(w(i));
      el.classList.toggle("live", w(i) > 0.5);
      el.inert = i !== cur;
    });
    rail.querySelectorAll("button").forEach((b, i) => b.classList.toggle("on", i === cur));
    const dist = (wide() ? 5.6 : 7.8) * (pageP < 1 ? L(1.7, 1, ease) : 1), elev = 0.42, az = -0.5 + T * 0.02 + sm.x * 0.25;
    cam.az = az;
    cam.look.set(wide() ? -1.1 : 0, (wide() ? -0.55 : 0.35) - sm.y * 0.2, 0);
    cam.p.set(Math.sin(az) * Math.cos(elev) * dist, Math.sin(elev) * dist - 0.4, Math.cos(az) * Math.cos(elev) * dist);
    cam.view = lookAt(cam.p, cam.look);
    const k = Math.min(1, dt * 2.2), w1 = w(1), w2 = w(2), w3 = w(3);
    spin += (tgt.spin - spin) * k;
    if (draining) {
      const freed = Math.min(WATER_N, Math.round(WATER_N * SS((T - drainT) / 9, 0, 1)));
      text("[data-vx=freed]", freed.toLocaleString("en"));
      if (freed >= WATER_N) text("[data-vx=own-state]", "done: 90,000 = 90,000");
    }
    const ringOn = Math.max(0, 1 - pageP * 1.7);
    ringR += (ruleR(rule) - ringR) * Math.min(1, dt * 6);
    hit = rule > 30 ? Math.max(hit * 0.98, 0.6) : hit * 0.94;
    heroRing.s = ringR;
    heroRing.p.set(0, funnel(ringR), 0);
    heroRing.o = ringOn;
    heroRing.c.set(0.9 + hit * 1.3, 0.95 - hit * 0.65, 1.05 - hit * 0.85);
    gates.forEach((s, i) => {
      const a = i * 2.094 + (T * SWIRL) / (ringR + 0.25);
      s.p.set(Math.cos(a) * ringR, funnel(ringR), Math.sin(a) * ringR);
      s.o = w(0);
    });
    effectsFrame(dt, w2);
    heroFrame(dt, w(0), Math.max(w(0), w1));
    stackFrame(dt, w1);
    const focus = Math.hypot(cam.p.x - cam.look.x, cam.p.y - cam.look.y, cam.p.z - cam.look.z);
    render({
      water: { uT: T, uPx: PX, uFocus: focus, uRing: ringOn > 0.02 ? ringR : 99, uHit: hit * (1 - w1), uSpin: spin, uVis: 1,
        uDrain: draining * w3 > 0.01 ? 1 : 0, uDrainT: drainT, uCalm: w(4) },
      drops: { uT: T, uPx: PX, uFocus: focus, uRing: ringR, uRule: (ringR - 0.5) / 0.065 + 1, uVis: w(0) },
    });
    raf = requestAnimationFrame(frame);
  }
  raf = requestAnimationFrame(frame);

  // Client-side navigation swaps the page without a reload, so the scene takes itself down.
  on(document, "vyrn:nav-start", () => {
    cancelAnimationFrame(raf);
    life.abort();
    canvas.remove();
    overlay.remove();
    rail.remove();
    document.documentElement.classList.remove("vx-live");
    gl.getExtension("WEBGL_lose_context")?.loseContext();
  });
  return true;
}
