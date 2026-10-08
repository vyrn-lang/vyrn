//! The shape sweep: every ownership shape of a store, a read and a removal,
//! run under the free audit.
//!
//! A shape is one operation on one place, in one context, under one root.
//! The generator walks a fixed, ordered space (no randomness):
//!
//!   - element type: a scalar, `String`, a record with a `String`, an array
//!     of `String`, an enum with a heap payload;
//!   - place: a name, `w.f`, `w.a.f`, `xs[i]`, `w.xs[i]`, `w.xss[0][i]`,
//!     `w.ins[i].f`, `w.m[k]` and `w.h.bag[i]` (a user container);
//!   - value stored: fresh, a read, a call that reads, a call that consumes,
//!     a `.copy()` and a take, each of the same place, a sibling place and a
//!     different container; a loop element, a payload binder and a borrow
//!     handed to a function that stores it, all into a name;
//!   - index form: a literal or a run-time variable, equal or different;
//!   - removal: `pop`, `swapRemove` and `Map.remove` on the container, and
//!     `pop`/`swapRemove` through a place that holds an array;
//!   - context: straight-line, a loop of three turns, an `if`;
//!   - root: a local, a `modify` parameter, module state.
//!
//! Every operation runs under each root; the context rotates with the
//! operation's ordinal, so the space is not the full product.
//!
//! `vyrn check` filters first. A refused shape ends there with its sentence.
//! An accepted shape runs under `VYRN_LEAK_CHECK=1` on the engine (`vyrn run`)
//! and on the wasm2c route (`vyrn build`), where it must exit 0 with the digest
//! the generator computed from its own model. A leak (exit 135), a double free
//! (exit 134), a different exit or a different digest is a failure. Accepted
//! shapes run in batches of `BATCH`; a batch that fails runs again shape by
//! shape, so each failure names one shape.
//!
//! The failures are held to `tests/pins/shapesweep-known.tsv`: a failure the
//! list does not name fails the gate, and a row that no longer fails is
//! reported as one to delete. `VYRN_PIN=write` deletes such rows and refuses
//! to add one. Failing programs are written under
//! `$VYRN_SHAPESWEEP_OUT` (default: the temp directory's `vyrn-shapesweep`).
//! `VYRN_SHAPESWEEP_ONLY=<text>` restricts the run to ids containing the text
//! and skips the list.

mod common;
use common::*;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

/// Shapes per program in the check and run phases. A batch that fails is split.
const BATCH: usize = 64;
/// `mk` arguments: a fresh store, the refill after a take, a payload binder's
/// value, and a key re-inserted after `Map.remove`.
const KS: i64 = 900;
const KR: i64 = 901;
const KP: i64 = 55;
const KI: i64 = 902;

// ---------------------------------------------------------------- types

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Ty {
    Int,
    Str,
    Rec,
    Arr,
    Enm,
}
const TYS: [Ty; 5] = [Ty::Int, Ty::Str, Ty::Rec, Ty::Arr, Ty::Enm];

impl Ty {
    fn tag(self) -> &'static str {
        match self {
            Ty::Int => "Int",
            Ty::Str => "Str",
            Ty::Rec => "Rec",
            Ty::Arr => "Arr",
            Ty::Enm => "Enm",
        }
    }
    fn name(self) -> &'static str {
        match self {
            Ty::Int => "Int64",
            Ty::Str => "String",
            Ty::Rec => "P",
            Ty::Arr => "Array<String>",
            Ty::Enm => "E",
        }
    }
    fn decls(self) -> &'static str {
        match self {
            Ty::Rec => "type P = { s: String, n: Int64 }",
            Ty::Enm => "type E = | A(String) | B(Int64)",
            _ => "",
        }
    }
    fn mk(self) -> &'static str {
        match self {
            Ty::Int => "fn mk(k: Int64) -> Int64 { return k }",
            Ty::Str => "fn mk(k: Int64) -> String { return \"v\" + k.toString() }",
            Ty::Rec => "fn mk(k: Int64) -> P { return P { s: \"p\" + k.toString(), n: k } }",
            Ty::Arr => {
                "fn mk(k: Int64) -> Array<String> { return [\"a\" + k.toString(), \"b\" + k.toString(), \"c\" + k.toString(), \"d\" + k.toString(), \"e\" + k.toString()] }"
            }
            Ty::Enm => {
                "fn mk(k: Int64) -> E {\n    if k % 2 == 0 { return A(\"e\" + k.toString()) }\n    return B(k)\n}"
            }
        }
    }
    fn show(self) -> &'static str {
        match self {
            Ty::Int => "fn show(x: Int64) -> String { return x.toString() }",
            Ty::Str => "fn show(x: String) -> String { return x.copy() }",
            Ty::Rec => "fn show(x: P) -> String { return x.s + \"/\" + x.n.toString() }",
            Ty::Arr => {
                "fn show(x: Array<String>) -> String {\n    let mut o = \"[\"\n    for e in x { o = o + e + \";\" }\n    return o + \"]\"\n}"
            }
            Ty::Enm => {
                "fn show(x: E) -> String {\n    return match x {\n        A(s) => \"A\" + s,\n        B(n) => \"B\" + n.toString(),\n    }\n}"
            }
        }
    }
    /// A heapless literal of the type, for module state and its reset.
    fn dl(self) -> &'static str {
        match self {
            Ty::Int => "0",
            Ty::Str => "\"\"",
            Ty::Rec => "P { s: \"\", n: 0 }",
            Ty::Arr => "[]",
            Ty::Enm => "B(0)",
        }
    }
    /// What `show(mk(k))` prints, computed on this side.
    fn shown(self, k: i64) -> String {
        match self {
            Ty::Int => k.to_string(),
            Ty::Str => format!("v{k}"),
            Ty::Rec => format!("p{k}/{k}"),
            Ty::Arr => format!("[a{k};b{k};c{k};d{k};e{k};]"),
            Ty::Enm if k % 2 == 0 => format!("Ae{k}"),
            Ty::Enm => format!("B{k}"),
        }
    }
}

fn wlit(ty: Ty) -> String {
    let d = ty.dl();
    let inn = format!("In {{ f: {d}, g: {d} }}");
    let h = "H { bag: Bag { data: [] } }";
    format!(
        "W {{ f: {d}, g: {d}, a: {inn}, b: {inn}, xs: [], ys: [], xss: [], ins: [], m: [:], h: {h}, k: {h} }}"
    )
}

fn prelude(ty: Ty) -> String {
    let t = ty.name();
    let text = r#"@DECLS@
type In = { f: @T@, g: @T@ }
type Bag = { data: Array<@T@> }
impl Index for Bag {
    fn at(read self, i: Int64) -> read @T@ { return self.data[i] }
    fn atSet(modify self, i: Int64) -> modify @T@ { return self.data[i] }
}
type H = { bag: Bag }
type W = { f: @T@, g: @T@, a: In, b: In, xs: Array<@T@>, ys: Array<@T@>, xss: Array<Array<@T@>>, ins: Array<In>, m: Map<String, @T@>, h: H, k: H }
type Cell = | Hold(@T@) | Nothing
fn idx(k: Int64) -> Int64 { return k }
fn key(k: Int64) -> String { return "k" + k.toString() }
@MK@
@SHOW@
fn rd(x: @T@) -> @T@ { return x.copy() }
fn eat(x: consume @T@) -> @T@ {
    let y = x.copy()
    return y
}
fn many(k: Int64) -> Array<@T@> { return [mk(k), mk(k + 1), mk(k + 2), mk(k + 3), mk(k + 4)] }
fn arr(xs: Array<@T@>) -> String {
    let mut o = "["
    for x in xs { o = o + show(x) + "," }
    return o + "]"
}
fn world() -> W {
    let mut m: Map<String, @T@> = [:]
    m["k0"] = mk(60)
    m["k1"] = mk(61)
    let mut ins: Array<In> = []
    ins.push(In { f: mk(40), g: mk(41) })
    ins.push(In { f: mk(42), g: mk(43) })
    ins.push(In { f: mk(44), g: mk(45) })
    ins.push(In { f: mk(46), g: mk(47) })
    ins.push(In { f: mk(48), g: mk(49) })
    let mut xss: Array<Array<@T@>> = []
    xss.push(many(20))
    xss.push(many(30))
    return W { f: mk(1), g: mk(2), a: In { f: mk(3), g: mk(4) }, b: In { f: mk(5), g: mk(6) }, xs: many(10), ys: many(70), xss: xss, ins: ins, m: m, h: H { bag: Bag { data: many(80) } }, k: H { bag: Bag { data: many(90) } } }
}
fn wlit() -> W { return @WLIT@ }
fn dig(w: W, s: @T@, t: @T@, xs: Array<@T@>, ys: Array<@T@>) -> String {
    let mut o = "s=" + show(s) + " t=" + show(t) + " xs=" + arr(xs) + " ys=" + arr(ys)
    o = o + " f=" + show(w.f) + " g=" + show(w.g)
    o = o + " a=" + show(w.a.f) + "," + show(w.a.g) + " b=" + show(w.b.f) + "," + show(w.b.g)
    o = o + " wxs=" + arr(w.xs) + " wys=" + arr(w.ys)
    o = o + " xss="
    for x in w.xss { o = o + arr(x) }
    o = o + " ins="
    for i in w.ins { o = o + show(i.f) + "," + show(i.g) + ";" }
    o = o + " m="
    for k in w.m.keys() {
        if let Some(v) = w.m[k] { o = o + k + ":" + show(v) + ";" }
    }
    o = o + " bag=" + arr(w.h.bag.data) + " bag2=" + arr(w.k.bag.data)
    return o
}
let mut gw = @WLIT@
let mut gs: @T@ = @DL@
let mut gt: @T@ = @DL@
let mut gxs: Array<@T@> = []
let mut gys: Array<@T@> = []
"#;
    text.replace("@DECLS@", ty.decls())
        .replace("@MK@", ty.mk())
        .replace("@SHOW@", ty.show())
        .replace("@WLIT@", &wlit(ty))
        .replace("@DL@", ty.dl())
        .replace("@T@", t)
}

// ---------------------------------------------------------------- the model

/// The generator's own picture of every place, as the `show` text of its value.
#[derive(Clone)]
struct Mdl {
    s: String,
    t: String,
    f: String,
    g: String,
    af: String,
    ag: String,
    bf: String,
    bg: String,
    xs: Vec<String>,
    ys: Vec<String>,
    wxs: Vec<String>,
    wys: Vec<String>,
    x0: Vec<String>,
    x1: Vec<String>,
    bag: Vec<String>,
    bag2: Vec<String>,
    ins: Vec<(String, String)>,
    mp: Vec<(String, String)>,
    res: String,
}

fn seq(ty: Ty, k: i64) -> Vec<String> {
    (k..k + 5).map(|k| ty.shown(k)).collect()
}

impl Mdl {
    fn new(ty: Ty) -> Mdl {
        let v = |k| ty.shown(k);
        Mdl {
            s: v(100),
            t: v(101),
            f: v(1),
            g: v(2),
            af: v(3),
            ag: v(4),
            bf: v(5),
            bg: v(6),
            xs: seq(ty, 110),
            ys: seq(ty, 120),
            wxs: seq(ty, 10),
            wys: seq(ty, 70),
            x0: seq(ty, 20),
            x1: seq(ty, 30),
            bag: seq(ty, 80),
            bag2: seq(ty, 90),
            ins: (0..5).map(|i| (v(40 + 2 * i), v(41 + 2 * i))).collect(),
            mp: vec![("k0".into(), v(60)), ("k1".into(), v(61))],
            res: String::new(),
        }
    }

    /// `dig`, as the program prints it, then the shape's `res`.
    fn digest(&self) -> String {
        let arr = |xs: &Vec<String>| {
            let mut o = String::from("[");
            for x in xs {
                o += x;
                o += ",";
            }
            o + "]"
        };
        let mut o = format!(
            "s={} t={} xs={} ys={}",
            self.s,
            self.t,
            arr(&self.xs),
            arr(&self.ys)
        );
        o += &format!(" f={} g={}", self.f, self.g);
        o += &format!(" a={},{} b={},{}", self.af, self.ag, self.bf, self.bg);
        o += &format!(" wxs={} wys={}", arr(&self.wxs), arr(&self.wys));
        o += " xss=";
        o += &arr(&self.x0);
        o += &arr(&self.x1);
        o += " ins=";
        for (f, g) in &self.ins {
            o += &format!("{f},{g};");
        }
        o += " m=";
        for (k, v) in &self.mp {
            o += &format!("{k}:{v};");
        }
        o += &format!(" bag={} bag2={}", arr(&self.bag), arr(&self.bag2));
        o + &self.res
    }
}

// ---------------------------------------------------------------- places

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Pk {
    Name,
    F1,
    F2,
    E1,
    Re,
    Ee,
    Ef,
    Mp,
    Ub,
}
const PKS: [Pk; 9] = [
    Pk::Name,
    Pk::F1,
    Pk::F2,
    Pk::E1,
    Pk::Re,
    Pk::Ee,
    Pk::Ef,
    Pk::Mp,
    Pk::Ub,
];

impl Pk {
    fn tag(self) -> &'static str {
        match self {
            Pk::Name => "name",
            Pk::F1 => "w.f",
            Pk::F2 => "w.a.f",
            Pk::E1 => "xs[i]",
            Pk::Re => "w.xs[i]",
            Pk::Ee => "w.xss[0][i]",
            Pk::Ef => "w.ins[i].f",
            Pk::Mp => "w.m[k]",
            Pk::Ub => "w.h.bag[i]",
        }
    }
    fn indexed(self) -> bool {
        !matches!(self, Pk::Name | Pk::F1 | Pk::F2)
    }
}

/// Which spelling of the place: the one written, a sibling in the same
/// container, or a place in a different container.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Wh {
    Write,
    Sib,
    Other,
}

/// An index form: literal or variable, and the write and read indices.
#[derive(Clone, Copy, Debug)]
struct Ix {
    var: bool,
    wi: usize,
    ri: usize,
    tag: &'static str,
}
const IXS: [Ix; 4] = [
    Ix {
        var: false,
        wi: 1,
        ri: 1,
        tag: "lit-eq",
    },
    Ix {
        var: false,
        wi: 2,
        ri: 0,
        tag: "lit-diff",
    },
    Ix {
        var: true,
        wi: 1,
        ri: 1,
        tag: "var-eq",
    },
    Ix {
        var: true,
        wi: 0,
        ri: 1,
        tag: "var-diff",
    },
];
const NOIX: Ix = Ix {
    var: false,
    wi: 0,
    ri: 0,
    tag: "-",
};
/// Index forms of a removal: a hit and a miss (or the last in range), by
/// literal and by variable.
const RIXS: [Ix; 4] = [
    Ix {
        var: false,
        wi: 0,
        ri: 0,
        tag: "lit-0",
    },
    Ix {
        var: false,
        wi: 2,
        ri: 0,
        tag: "lit-2",
    },
    Ix {
        var: true,
        wi: 1,
        ri: 0,
        tag: "var-1",
    },
    Ix {
        var: true,
        wi: 2,
        ri: 0,
        tag: "var-2",
    },
];

/// The names a root gives the five variables.
struct Names {
    w: &'static str,
    s: &'static str,
    t: &'static str,
    xs: &'static str,
    ys: &'static str,
}
const LOCAL: Names = Names {
    w: "w",
    s: "s",
    t: "t",
    xs: "xs",
    ys: "ys",
};
const GLOBAL: Names = Names {
    w: "gw",
    s: "gs",
    t: "gt",
    xs: "gxs",
    ys: "gys",
};

fn names(root: Root) -> &'static Names {
    if root == Root::Global {
        &GLOBAL
    } else {
        &LOCAL
    }
}

fn ptext(p: Pk, wh: Wh, ix: Ix, n: &Names) -> String {
    let i = match (wh, ix.var) {
        (Wh::Write, true) => "i".to_string(),
        (_, true) => "j".to_string(),
        (Wh::Write, false) => ix.wi.to_string(),
        (_, false) => ix.ri.to_string(),
    };
    let k = match (wh, ix.var) {
        (Wh::Write, true) => "kw".to_string(),
        (_, true) => "kr".to_string(),
        (Wh::Write, false) => format!("\"k{}\"", ix.wi),
        (_, false) => format!("\"k{}\"", ix.ri),
    };
    let w = n.w;
    match (p, wh) {
        (Pk::Name, Wh::Write) => n.s.to_string(),
        (Pk::Name, Wh::Sib) => n.t.to_string(),
        (Pk::Name, Wh::Other) => format!("{}[{i}]", n.ys),
        (Pk::F1, Wh::Write) => format!("{w}.f"),
        (Pk::F1, Wh::Sib) => format!("{w}.g"),
        (Pk::F1, Wh::Other) => format!("{w}.a.f"),
        (Pk::F2, Wh::Write) => format!("{w}.a.f"),
        (Pk::F2, Wh::Sib) => format!("{w}.a.g"),
        (Pk::F2, Wh::Other) => format!("{w}.b.f"),
        (Pk::E1, Wh::Write | Wh::Sib) => format!("{}[{i}]", n.xs),
        (Pk::E1, Wh::Other) => format!("{}[{i}]", n.ys),
        (Pk::Re, Wh::Write | Wh::Sib) => format!("{w}.xs[{i}]"),
        (Pk::Re, Wh::Other) => format!("{w}.ys[{i}]"),
        (Pk::Ee, Wh::Write | Wh::Sib) => format!("{w}.xss[0][{i}]"),
        (Pk::Ee, Wh::Other) => format!("{w}.xss[1][{i}]"),
        (Pk::Ef, Wh::Write | Wh::Sib) => format!("{w}.ins[{i}].f"),
        (Pk::Ef, Wh::Other) => format!("{w}.a.f"),
        (Pk::Mp, Wh::Write | Wh::Sib) => format!("{w}.m[{k}]"),
        (Pk::Mp, Wh::Other) => format!("{}[{i}]", n.ys),
        (Pk::Ub, Wh::Write | Wh::Sib) => format!("{w}.h.bag[{i}]"),
        (Pk::Ub, Wh::Other) => format!("{w}.k.bag[{i}]"),
    }
}

/// The model's cell for a place. A map cell is made on first write.
fn slot<'a>(m: &'a mut Mdl, p: Pk, wh: Wh, ix: Ix) -> &'a mut String {
    let i = if wh == Wh::Write { ix.wi } else { ix.ri };
    match (p, wh) {
        (Pk::Name, Wh::Write) => &mut m.s,
        (Pk::Name, Wh::Sib) => &mut m.t,
        (Pk::Name, Wh::Other) => &mut m.ys[i],
        (Pk::F1, Wh::Write) => &mut m.f,
        (Pk::F1, Wh::Sib) => &mut m.g,
        (Pk::F1, Wh::Other) => &mut m.af,
        (Pk::F2, Wh::Write) => &mut m.af,
        (Pk::F2, Wh::Sib) => &mut m.ag,
        (Pk::F2, Wh::Other) => &mut m.bf,
        (Pk::E1, Wh::Write | Wh::Sib) => &mut m.xs[i],
        (Pk::E1, Wh::Other) => &mut m.ys[i],
        (Pk::Re, Wh::Write | Wh::Sib) => &mut m.wxs[i],
        (Pk::Re, Wh::Other) => &mut m.wys[i],
        (Pk::Ee, Wh::Write | Wh::Sib) => &mut m.x0[i],
        (Pk::Ee, Wh::Other) => &mut m.x1[i],
        (Pk::Ef, Wh::Write | Wh::Sib) => &mut m.ins[i].0,
        (Pk::Ef, Wh::Other) => &mut m.af,
        (Pk::Mp, Wh::Write | Wh::Sib) => {
            let key = format!("k{i}");
            let at = match m.mp.iter().position(|(k, _)| *k == key) {
                Some(at) => at,
                None => {
                    m.mp.push((key, String::new()));
                    m.mp.len() - 1
                }
            };
            &mut m.mp[at].1
        }
        (Pk::Mp, Wh::Other) => &mut m.ys[i],
        (Pk::Ub, Wh::Write | Wh::Sib) => &mut m.bag[i],
        (Pk::Ub, Wh::Other) => &mut m.bag2[i],
    }
}

/// Whether a map place's key is present.
fn map_has(m: &Mdl, wh: Wh, ix: Ix) -> bool {
    let i = if wh == Wh::Write { ix.wi } else { ix.ri };
    m.mp.iter().any(|(k, _)| *k == format!("k{i}"))
}

// ---------------------------------------------------------------- operations

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Src {
    Same,
    Sib,
    Other,
}
impl Src {
    fn wh(self) -> Wh {
        match self {
            Src::Same => Wh::Write,
            Src::Sib => Wh::Sib,
            Src::Other => Wh::Other,
        }
    }
    fn tag(self) -> &'static str {
        match self {
            Src::Same => "same",
            Src::Sib => "sib",
            Src::Other => "other",
        }
    }
}
const SRCS: [Src; 3] = [Src::Same, Src::Sib, Src::Other];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Vk {
    Fresh,
    Read(Src),
    Copy(Src),
    Rd(Src),
    EatBorrow(Src),
    EatCopy(Src),
    Take(Src),
    /// A borrow handed to a function that stores it into its own `let mut` name.
    Adopt(Src),
    LoopElem,
    Payload,
}

impl Vk {
    fn tag(self) -> String {
        match self {
            Vk::Fresh => "fresh".into(),
            Vk::Read(s) => format!("read-{}", s.tag()),
            Vk::Copy(s) => format!("copy-{}", s.tag()),
            Vk::Rd(s) => format!("call-read-{}", s.tag()),
            Vk::EatBorrow(s) => format!("call-consume-{}", s.tag()),
            Vk::EatCopy(s) => format!("call-consume-copy-{}", s.tag()),
            Vk::Take(s) => format!("take-{}", s.tag()),
            Vk::Adopt(s) => format!("adopt-{}", s.tag()),
            Vk::LoopElem => "loop-elem".into(),
            Vk::Payload => "payload".into(),
        }
    }
    /// The values every build must accept: a fresh value, and a read that
    /// reaches the place through a `.copy()` or a call that returns a value.
    fn legal(self) -> bool {
        matches!(self, Vk::Fresh | Vk::Copy(_) | Vk::Rd(_) | Vk::EatCopy(_))
    }
}

fn values(p: Pk) -> Vec<Vk> {
    let mut v = vec![Vk::Fresh];
    for s in SRCS {
        v.push(Vk::Read(s));
    }
    for s in SRCS {
        v.push(Vk::Copy(s));
    }
    for s in SRCS {
        v.push(Vk::Rd(s));
    }
    for s in SRCS {
        v.push(Vk::EatBorrow(s));
    }
    for s in SRCS {
        v.push(Vk::EatCopy(s));
    }
    if !p.indexed() {
        v.push(Vk::Take(Src::Same));
        v.push(Vk::Take(Src::Sib));
    }
    if p == Pk::Name {
        for s in SRCS {
            v.push(Vk::Adopt(s));
        }
        v.push(Vk::LoopElem);
        v.push(Vk::Payload);
    }
    v
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Ck {
    E1,
    Re,
    Ee,
    Ub,
    Ef,
}
const CKS: [Ck; 5] = [Ck::E1, Ck::Re, Ck::Ee, Ck::Ub, Ck::Ef];

impl Ck {
    fn tag(self) -> &'static str {
        match self {
            Ck::E1 => "xs",
            Ck::Re => "w.xs",
            Ck::Ee => "w.xss[0]",
            Ck::Ub => "w.h.bag.data",
            Ck::Ef => "w.ins",
        }
    }
    fn text(self, n: &Names) -> String {
        match self {
            Ck::E1 => n.xs.to_string(),
            Ck::Re => format!("{}.xs", n.w),
            Ck::Ee => format!("{}.xss[0]", n.w),
            Ck::Ub => format!("{}.h.bag.data", n.w),
            Ck::Ef => format!("{}.ins", n.w),
        }
    }
    fn vec<'a>(self, m: &'a mut Mdl) -> &'a mut Vec<String> {
        match self {
            Ck::E1 => &mut m.xs,
            Ck::Re => &mut m.wxs,
            Ck::Ee => &mut m.x0,
            Ck::Ub => &mut m.bag,
            Ck::Ef => unreachable!("ins holds pairs"),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Rm {
    Pop,
    Swap,
}
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Rs {
    Discard,
    Bind,
    Store,
}

#[derive(Clone, Copy, Debug)]
enum Op {
    Store {
        p: Pk,
        v: Vk,
        ix: Ix,
    },
    /// `pop` or `swapRemove(ix)` on a container.
    RemC {
        c: Ck,
        rm: Rm,
        rs: Rs,
        ix: Ix,
    },
    /// `Map.remove(key)`; `Store` re-inserts the key afterwards.
    RemM {
        rs: Rs,
        ix: Ix,
    },
    /// `pop` or `swapRemove(ix)` on an array-valued place (`Ty::Arr` only).
    RemLeaf {
        p: Pk,
        rm: Rm,
        rs: Rs,
        pix: Ix,
        ix: Ix,
    },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Cx {
    Straight,
    Loop,
    If,
}
const CXS: [Cx; 3] = [Cx::Straight, Cx::Loop, Cx::If];
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Root {
    Local,
    Param,
    Global,
}
const ROOTS: [Root; 3] = [Root::Local, Root::Param, Root::Global];

struct Shape {
    id: String,
    ty: Ty,
    op: Op,
    cx: Cx,
    root: Root,
    legal: bool,
}

// ---------------------------------------------------------------- the model's step

fn leaf_items(s: &str) -> Vec<String> {
    s.trim_start_matches('[')
        .trim_end_matches(']')
        .split(';')
        .filter(|x| !x.is_empty())
        .map(str::to_string)
        .collect()
}
fn leaf_text(items: &[String]) -> String {
    let mut o = String::from("[");
    for i in items {
        o += i;
        o += ";";
    }
    o + "]"
}

fn apply(op: &Op, ty: Ty, m: &mut Mdl) {
    match *op {
        Op::Store { p, v, ix } => {
            let val = match v {
                Vk::Fresh => Some(ty.shown(KS)),
                Vk::LoopElem => m.ys.last().cloned(),
                Vk::Payload => Some(ty.shown(KP)),
                Vk::Read(s)
                | Vk::Copy(s)
                | Vk::Rd(s)
                | Vk::EatBorrow(s)
                | Vk::EatCopy(s)
                | Vk::Take(s)
                | Vk::Adopt(s) => {
                    if p == Pk::Mp && s != Src::Other && !map_has(m, s.wh(), ix) {
                        None
                    } else {
                        Some(slot(m, p, s.wh(), ix).clone())
                    }
                }
            };
            if let Some(val) = val {
                *slot(m, p, Wh::Write, ix) = val;
                if matches!(v, Vk::Take(Src::Sib)) {
                    *slot(m, p, Wh::Sib, ix) = ty.shown(KR);
                }
            }
        }
        Op::RemC { c, rm, rs, ix } => {
            if c == Ck::Ef {
                let got = match rm {
                    Rm::Pop => m.ins.pop(),
                    Rm::Swap => {
                        let last = m.ins.len() - 1;
                        m.ins.swap(ix.wi, last);
                        m.ins.pop()
                    }
                };
                if rs == Rs::Bind {
                    match (rm, got) {
                        (Rm::Pop, None) => m.res += " r=none",
                        (_, Some((f, g))) => m.res += &format!(" r={f},{g};"),
                        (Rm::Swap, None) => unreachable!(),
                    }
                }
                return;
            }
            let got = match rm {
                Rm::Pop => c.vec(m).pop(),
                Rm::Swap => {
                    let v = c.vec(m);
                    let last = v.len() - 1;
                    v.swap(ix.wi, last);
                    v.pop()
                }
            };
            match (rs, got) {
                (Rs::Bind, Some(x)) => m.res += &format!(" r={x}"),
                (Rs::Bind, None) => m.res += " r=none",
                (Rs::Store, Some(x)) => m.f = x,
                (Rs::Store, None) => {}
                (Rs::Discard, _) => {}
            }
        }
        Op::RemM { rs, ix } => {
            let key = format!("k{}", ix.wi);
            let at = m.mp.iter().position(|(k, _)| *k == key);
            if let Some(at) = at {
                m.mp.remove(at);
            }
            match rs {
                Rs::Bind => m.res += if at.is_some() { " ok" } else { " no" },
                Rs::Store => m.mp.push((key, ty.shown(KI))),
                Rs::Discard => {}
            }
        }
        Op::RemLeaf { p, rm, rs, pix, ix } => {
            let cell = slot(m, p, Wh::Write, pix);
            let mut items = leaf_items(cell);
            let got = match rm {
                Rm::Pop => items.pop(),
                Rm::Swap => {
                    let last = items.len() - 1;
                    items.swap(ix.wi, last);
                    items.pop()
                }
            };
            *cell = leaf_text(&items);
            if rs == Rs::Bind {
                match got {
                    Some(x) => m.res += &format!(" r={x}"),
                    None => m.res += " r=none",
                }
            }
        }
    }
}

// ---------------------------------------------------------------- the program text

fn expr(v: Vk, src: &str, name_take: bool) -> String {
    match v {
        Vk::Fresh => format!("mk({KS})"),
        Vk::Read(_) => src.to_string(),
        Vk::Copy(_) => format!("{src}.copy()"),
        Vk::Rd(_) => format!("rd({src})"),
        Vk::EatBorrow(_) => format!("eat({src})"),
        Vk::EatCopy(_) => format!("eat({src}.copy())"),
        Vk::Adopt(_) => format!("adopt({src})"),
        Vk::Take(_) if name_take => format!("eat({src})"),
        Vk::Take(_) => format!("eat(consume {src})"),
        Vk::LoopElem | Vk::Payload => unreachable!(),
    }
}

/// The element of `Ck`'s container, as `show` text.
fn show_el(c: Option<Ck>, x: &str) -> String {
    match c {
        None => x.to_string(),
        Some(Ck::Ef) => format!("show({x}.f) + \",\" + show({x}.g) + \";\""),
        Some(_) => format!("show({x})"),
    }
}

fn emit(op: &Op, n: &Names) -> Vec<String> {
    match *op {
        Op::Store { p, v, ix } => {
            // A map takes its key: a variable key is copied at the store.
            let place = ptext(p, Wh::Write, ix, n).replace("[kw]", "[kw.copy()]");
            match v {
                Vk::LoopElem => vec![format!("for x in {} {{ {place} = x }}", n.ys)],
                Vk::Payload => vec![
                    format!("let e = Hold(mk({KP}))"),
                    format!("if let Hold(x) = e {{ {place} = x }}"),
                ],
                Vk::Fresh => vec![format!("{place} = mk({KS})")],
                Vk::Read(s)
                | Vk::Copy(s)
                | Vk::Rd(s)
                | Vk::EatBorrow(s)
                | Vk::EatCopy(s)
                | Vk::Take(s)
                | Vk::Adopt(s) => {
                    let src = ptext(p, s.wh(), ix, n);
                    let wrap = p == Pk::Mp && s != Src::Other;
                    let name_take = p == Pk::Name;
                    let mut out = Vec::new();
                    let from = if wrap { "x" } else { src.as_str() };
                    if wrap {
                        out.push(format!("if let Some(x) = {src} {{"));
                    }
                    out.push(format!("{place} = {}", expr(v, from, name_take)));
                    if matches!(v, Vk::Take(Src::Sib)) {
                        out.push(format!("{src} = mk({KR})"));
                    }
                    if wrap {
                        out.push("}".into());
                    }
                    out
                }
            }
        }
        Op::RemC { c, rm, rs, ix } => {
            let ct = c.text(n);
            let call = match rm {
                Rm::Pop => format!("{ct}.pop()"),
                Rm::Swap => format!("{ct}.swapRemove({})", swap_at(ix)),
            };
            match (rm, rs) {
                (_, Rs::Discard) => vec![call],
                (Rm::Pop, Rs::Bind) => vec![
                    format!("let r = {call}"),
                    format!(
                        "if let Some(v) = r {{ res = res + \" r=\" + {} }} else {{ res = res + \" r=none\" }}",
                        show_el(Some(c), "v")
                    ),
                ],
                (Rm::Swap, Rs::Bind) => vec![
                    format!("let r = {call}"),
                    format!("res = res + \" r=\" + {}", show_el(Some(c), "r")),
                ],
                (Rm::Pop, Rs::Store) => vec![format!("if let Some(v) = {call} {{ {}.f = v }}", n.w)],
                (Rm::Swap, Rs::Store) => vec![format!("{}.f = {call}", n.w)],
            }
        }
        Op::RemM { rs, ix } => {
            let k = if ix.var {
                "kw".to_string()
            } else {
                format!("\"k{}\"", ix.wi)
            };
            let call = format!("{}.m.remove({k})", n.w);
            match rs {
                Rs::Discard => vec![call],
                Rs::Bind => vec![
                    format!("let ok = {call}"),
                    "if ok { res = res + \" ok\" } else { res = res + \" no\" }".into(),
                ],
                Rs::Store => vec![
                    call,
                    format!("{}.m[{}] = mk({KI})", n.w, k.replace("kw", "kw.copy()")),
                ],
            }
        }
        Op::RemLeaf { p, rm, rs, pix, ix } => {
            let place = ptext(p, Wh::Write, pix, n);
            let call = match rm {
                Rm::Pop => format!("{place}.pop()"),
                Rm::Swap => format!("{place}.swapRemove({})", swap_at(ix)),
            };
            match (rm, rs) {
                (_, Rs::Discard | Rs::Store) => vec![call],
                (Rm::Pop, Rs::Bind) => vec![
                    format!("let r = {call}"),
                    "if let Some(v) = r { res = res + \" r=\" + v } else { res = res + \" r=none\" }".into(),
                ],
                (Rm::Swap, Rs::Bind) => {
                    vec![format!("let r = {call}"), "res = res + \" r=\" + r".into()]
                }
            }
        }
    }
}

fn swap_at(ix: Ix) -> String {
    if ix.var {
        "q".to_string()
    } else {
        ix.wi.to_string()
    }
}

/// The `let`s that make an index a run-time value.
fn lets(op: &Op) -> Vec<String> {
    let mut out = Vec::new();
    let elem = |out: &mut Vec<String>, ix: Ix| {
        if ix.var {
            out.push(format!("let i = idx({})", ix.wi));
            out.push(format!("let j = idx({})", ix.ri));
        }
    };
    let keys = |out: &mut Vec<String>, ix: Ix| {
        if ix.var {
            out.push(format!("let kw = key({})", ix.wi));
            out.push(format!("let kr = key({})", ix.ri));
        }
    };
    let swap = |out: &mut Vec<String>, ix: Ix| {
        if ix.var {
            out.push(format!("let q = idx({})", ix.wi));
        }
    };
    match *op {
        Op::Store { p: Pk::Mp, ix, .. } => {
            keys(&mut out, ix);
            elem(&mut out, ix);
        }
        Op::Store { ix, .. } => elem(&mut out, ix),
        Op::RemC { rm, ix, .. } => {
            if rm == Rm::Swap {
                swap(&mut out, ix);
            }
        }
        Op::RemM { ix, .. } => keys(&mut out, ix),
        Op::RemLeaf { pix, ix, rm, .. } => {
            elem(&mut out, pix);
            if rm == Rm::Swap {
                swap(&mut out, ix);
            }
        }
    }
    out
}

fn wrap_ctx(cx: Cx, body: Vec<String>) -> Vec<String> {
    match cx {
        Cx::Straight => body,
        Cx::Loop => {
            let mut out = vec!["let mut lp = 0".into(), "while lp < 3 {".into()];
            out.extend(body);
            out.push("lp = lp + 1".into());
            out.push("}".into());
            out
        }
        Cx::If => {
            let mut out = vec!["if idx(1) == 1 {".to_string()];
            out.extend(body);
            out.push("}".into());
            out
        }
    }
}

/// The text of one shape's function (and its callee under a `modify` root).
fn emit_shape(sh: &Shape, nth: usize) -> String {
    let t = sh.ty.name();
    let n = names(sh.root);
    let mut body = lets(&sh.op);
    body.extend(wrap_ctx(sh.cx, emit(&sh.op, n)));
    let text = |lines: &[String]| {
        lines
            .iter()
            .map(|l| format!("    {l}\n"))
            .collect::<String>()
    };
    let init = format!(
        "    let mut w = world()\n    let mut s = mk(100)\n    let mut t = mk(101)\n    let mut xs = many(110)\n    let ys = many(120)\n"
    );
    match sh.root {
        Root::Local => format!(
            "fn sh{nth}() -> String {{\n{init}    let mut res = \"\"\n{}    return dig(w, s, t, xs, ys) + res\n}}\n",
            text(&body)
        ),
        Root::Param => format!(
            "fn core{nth}(w: modify W, s: modify {t}, t: modify {t}, xs: modify Array<{t}>, ys: Array<{t}>) -> String {{\n    let mut res = \"\"\n{}    return res\n}}\nfn sh{nth}() -> String {{\n{init}    let res = core{nth}(w, s, t, xs, ys)\n    return dig(w, s, t, xs, ys) + res\n}}\n",
            text(&body)
        ),
        Root::Global => format!(
            "fn sh{nth}() -> String {{\n    gw = world()\n    gs = mk(100)\n    gt = mk(101)\n    gxs = many(110)\n    gys = many(120)\n    let mut res = \"\"\n{}    let d = dig(gw, gs, gt, gxs, gys) + res\n    gw = wlit()\n    gs = {}\n    gt = {}\n    gxs = []\n    gys = []\n    return d\n}}\n",
            text(&body),
            sh.ty.dl(),
            sh.ty.dl()
        ),
    }
}

fn expected(sh: &Shape) -> String {
    let mut m = Mdl::new(sh.ty);
    let turns = if sh.cx == Cx::Loop { 3 } else { 1 };
    for _ in 0..turns {
        apply(&sh.op, sh.ty, &mut m);
    }
    m.digest()
}

const ADOPT: &str = "fn adopt(b: @T@) -> @T@ {
    let mut l = mk(7)
    l = b
    return l
}
";

fn program(ty: Ty, shapes: &[(usize, &Shape)]) -> String {
    let mut out = prelude(ty);
    let body: String = shapes
        .iter()
        .map(|(nth, sh)| emit_shape(sh, *nth))
        .collect();
    // Only a program that adopts declares `adopt`: the store inside it is a
    // borrow into a `let mut` name, which a build may refuse as a whole.
    if body.contains("adopt(") {
        out += &ADOPT.replace("@T@", ty.name());
    }
    out += &body;
    out += "fn main() -> Int64 {\n";
    for (nth, sh) in shapes {
        out += &format!("    print(\"{}: \" + sh{nth}())\n", sh.id);
    }
    out += "    return 0\n}\n";
    out
}

fn expected_out(shapes: &[(usize, &Shape)]) -> String {
    shapes
        .iter()
        .map(|(_, sh)| format!("{}: {}\n", sh.id, expected(sh)))
        .collect()
}

// ---------------------------------------------------------------- the space

fn space() -> Vec<Shape> {
    let mut ops: Vec<(Ty, Op, String, bool)> = Vec::new();
    for ty in TYS {
        for p in PKS {
            let ixs: &[Ix] = if p.indexed() { &IXS } else { &[NOIX] };
            for v in values(p) {
                for ix in ixs {
                    ops.push((
                        ty,
                        Op::Store { p, v, ix: *ix },
                        format!("store.{}.{}.{}", p.tag(), v.tag(), ix.tag),
                        v.legal(),
                    ));
                }
            }
        }
        let rms = [(Rm::Pop, "pop"), (Rm::Swap, "swap")];
        for c in CKS {
            for (rm, rt) in rms {
                let ixs: &[Ix] = if rm == Rm::Swap { &RIXS } else { &[NOIX] };
                for ix in ixs {
                    for (rs, st) in [
                        (Rs::Discard, "discard"),
                        (Rs::Bind, "bind"),
                        (Rs::Store, "store"),
                    ] {
                        if c == Ck::Ef && rs == Rs::Store {
                            continue;
                        }
                        ops.push((
                            ty,
                            Op::RemC { c, rm, rs, ix: *ix },
                            format!("remove.{}.{rt}.{st}.{}", c.tag(), ix.tag),
                            true,
                        ));
                    }
                }
            }
        }
        for ix in RIXS {
            for (rs, st) in [
                (Rs::Discard, "discard"),
                (Rs::Bind, "bind"),
                (Rs::Store, "reinsert"),
            ] {
                ops.push((
                    ty,
                    Op::RemM { rs, ix },
                    format!("remove.w.m.{st}.{}", ix.tag),
                    true,
                ));
            }
        }
        if ty == Ty::Arr {
            for p in PKS {
                if p == Pk::Mp {
                    continue;
                }
                let pixs: &[Ix] = if p.indexed() { &IXS } else { &[NOIX] };
                for pix in pixs {
                    let leaf = [
                        (Rm::Pop, Rs::Discard, NOIX, "pop.discard"),
                        (Rm::Pop, Rs::Bind, NOIX, "pop.bind"),
                        (Rm::Swap, Rs::Discard, RIXS[0], "swap-lit.discard"),
                        (Rm::Swap, Rs::Bind, RIXS[2], "swap-var.bind"),
                    ];
                    for (rm, rs, ix, tag) in leaf {
                        ops.push((
                            ty,
                            Op::RemLeaf {
                                p,
                                rm,
                                rs,
                                pix: *pix,
                                ix,
                            },
                            format!("remove-leaf.{}.{tag}.{}", p.tag(), pix.tag),
                            false,
                        ));
                    }
                }
            }
        }
    }
    let mut out = Vec::new();
    for (n, (ty, op, tag, legal)) in ops.iter().enumerate() {
        for (r, root) in ROOTS.into_iter().enumerate() {
            let cx = CXS[(n + r) % 3];
            let cxn = ["straight", "loop", "if"][cx as usize];
            let rn = ["local", "param", "global"][root as usize];
            out.push(Shape {
                id: format!("{}.{tag}.{cxn}.{rn}", ty.tag()),
                ty: *ty,
                op: *op,
                cx,
                root,
                legal: *legal,
            });
        }
    }
    out
}

// ---------------------------------------------------------------- running

#[derive(Default, Clone)]
struct Outcome {
    /// Sorted failure kinds; empty means the shape passed or was refused as allowed.
    kinds: Vec<String>,
    /// The refusal sentence, normalized, when `vyrn check` refused.
    refusal: Option<String>,
    detail: String,
}

fn sentence(stderr: &str) -> String {
    let line = stderr
        .lines()
        .find(|l| !l.contains(": warning: ") && !l.trim().is_empty())
        .unwrap_or("");
    // Drop `file:line:col: `, then the names between backticks.
    let mut rest = line;
    if let Some(at) = line.find(".vyrn:") {
        rest = line[at + ".vyrn:".len()..]
            .trim_start_matches(|c: char| c.is_ascii_digit() || c == ':')
            .trim_start();
    }
    let mut out = String::new();
    let mut inside = false;
    for c in rest.chars() {
        if c == '`' {
            inside = !inside;
            if inside {
                out += "`_`";
            }
        } else if !inside {
            out.push(c);
        }
    }
    out
}

fn judge(
    leg: &str,
    code: Option<i32>,
    stdout: &str,
    stderr: &str,
    want: &str,
    kinds: &mut Vec<String>,
    detail: &mut String,
) {
    let bad = match code {
        Some(0) => None,
        Some(134) => Some("double-free"),
        Some(135) => Some("leak"),
        Some(_) => Some("exit"),
        None => Some("signal"),
    };
    if let Some(k) = bad {
        kinds.push(format!("{k}@{leg}"));
        detail.push_str(&format!("[{leg}] exit {code:?}\n{stderr}\n"));
    }
    if !matches!(code, Some(134)) && stdout != want {
        kinds.push(format!("output@{leg}"));
        detail.push_str(&format!(
            "[{leg}] output differs\n  want: {want}  got:  {stdout}\n"
        ));
    }
}

struct Rig {
    dir: PathBuf,
    route: bool,
    engine_us: AtomicUsize,
    route_us: AtomicUsize,
}

impl Rig {
    fn check(&self, src: &str, tag: &str) -> (bool, String) {
        let file = self.dir.join(format!("{tag}.vyrn"));
        std::fs::write(&file, src).unwrap();
        let mut cmd = vyrn();
        cmd.arg("check").arg(&file);
        let r = run_io(cmd, &self.dir, Path::new("/nonexistent"));
        (r.status.success(), norm(&r.stderr) + &norm(&r.stdout))
    }

    /// Runs `src` on the engine; returns the failure kinds and a detail text.
    fn engine(&self, src: &str, tag: &str, want: &str) -> (Vec<String>, String) {
        let file = self.dir.join(format!("{tag}.vyrn"));
        std::fs::write(&file, src).unwrap();
        let (mut kinds, mut detail) = (Vec::new(), String::new());
        let t0 = std::time::Instant::now();
        let mut cmd = vyrn();
        cmd.env("VYRN_LEAK_CHECK", "1").arg("run").arg(&file);
        let r = run_io(cmd, &self.dir, Path::new("/nonexistent"));
        self.engine_us
            .fetch_add(t0.elapsed().as_micros() as usize, Ordering::Relaxed);
        judge(
            "engine",
            r.status.code(),
            &norm(&r.stdout),
            &runtime_err(&r.stderr),
            want,
            &mut kinds,
            &mut detail,
        );
        kinds.sort();
        kinds.dedup();
        (kinds, detail)
    }

    /// Builds `src` on the wasm2c route and runs it; the same result as `engine`.
    fn route(&self, src: &str, tag: &str, want: &str) -> (Vec<String>, String) {
        let file = self.dir.join(format!("{tag}.vyrn"));
        std::fs::write(&file, src).unwrap();
        let (mut kinds, mut detail) = (Vec::new(), String::new());
        if !self.route {
            return (kinds, detail);
        }
        let t0 = std::time::Instant::now();
        let exe = self.dir.join(format!("{tag}.exe"));
        let build = vyrn()
            .env("VYRN_LEAK_CHECK", "1")
            .arg("build")
            .arg(&file)
            .arg("-o")
            .arg(&exe)
            .output()
            .expect("build");
        if !build.status.success() {
            kinds.push("build@route".into());
            detail.push_str(&format!(
                "[route] build failed
{}
",
                norm(&build.stderr)
            ));
        } else {
            let r = run_io(Command::new(&exe), &self.dir, Path::new("/nonexistent"));
            judge(
                "route",
                r.status.code(),
                &norm(&r.stdout),
                &runtime_err(&r.stderr),
                want,
                &mut kinds,
                &mut detail,
            );
            let _ = std::fs::remove_file(&exe);
        }
        self.route_us
            .fetch_add(t0.elapsed().as_micros() as usize, Ordering::Relaxed);
        kinds.sort();
        kinds.dedup();
        (kinds, detail)
    }
}

fn for_each_parallel<T: Sync, R: Send>(items: &[T], f: impl Fn(usize, &T) -> R + Sync) -> Vec<R> {
    let next = AtomicUsize::new(0);
    let done = Mutex::new(Vec::new());
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get());
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                let Some(item) = items.get(i) else { break };
                let r = f(i, item);
                done.lock().unwrap().push((i, r));
            });
        }
    });
    let mut done = done.into_inner().unwrap();
    done.sort_by_key(|(i, _)| *i);
    done.into_iter().map(|(_, r)| r).collect()
}

fn fnv(h: &mut u64, s: &str) {
    for b in s.bytes() {
        *h ^= b as u64;
        *h = h.wrapping_mul(0x100000001b3);
    }
}

fn known_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/pins/shapesweep-known.tsv")
}

/// The known failures as `(id, kinds)` rows, and the comment lines.
fn known() -> BTreeMap<String, String> {
    let text = std::fs::read_to_string(known_path()).unwrap_or_default();
    text.lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .map(|l| {
            let (id, kinds) = l.split_once('\t').expect("a known row is `id<TAB>kinds`");
            (id.to_string(), kinds.to_string())
        })
        .collect()
}

#[test]
#[ignore = "generates and runs the ownership shapes, twice each; run explicitly: cargo test -p vyrn-cli --release --test shapesweep -- --ignored --nocapture"]
fn every_ownership_shape_is_refused_or_runs_clean() {
    let started = std::time::Instant::now();
    let only = std::env::var("VYRN_SHAPESWEEP_ONLY").ok();
    let dir = scratch("shapesweep");
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let route = vyrn_codegen::toolchain::wasm2c_from(&root)
        .ok()
        .flatten()
        .is_some()
        && vyrn_codegen::toolchain::simde_from(&root).is_some()
        && vyrn_codegen::toolchain::find_clang().is_some();
    if !route {
        eprintln!("SKIP the route's leg: clang, wabt or simde is missing");
    }
    let rig = Rig {
        dir: dir.to_path_buf(),
        route,
        engine_us: AtomicUsize::new(0),
        route_us: AtomicUsize::new(0),
    };
    let out_dir = std::env::var_os("VYRN_SHAPESWEEP_OUT")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("vyrn-shapesweep"));
    let _ = std::fs::remove_dir_all(&out_dir);

    let mut shapes = space();
    if let Some(only) = &only {
        shapes.retain(|s| s.id.contains(only.as_str()));
    }
    let mut ids: Vec<&str> = shapes.iter().map(|s| s.id.as_str()).collect();
    ids.sort();
    let before = ids.len();
    ids.dedup();
    assert_eq!(before, ids.len(), "two shapes share an id");
    eprintln!(
        "shapesweep: {} shapes, enumeration exhaustive and ordered, no seed",
        shapes.len()
    );

    // Filter with `vyrn check`. A shape the language must accept is checked
    // with its neighbours of one type, 24 to a program, and alone only when
    // that program is refused; any other shape is checked alone, because
    // refusing it is expected.
    let mut jobs: Vec<Vec<usize>> = Vec::new();
    for ty in TYS {
        let safe: Vec<usize> = (0..shapes.len())
            .filter(|&i| shapes[i].ty == ty && shapes[i].legal)
            .collect();
        jobs.extend(safe.chunks(BATCH).map(<[usize]>::to_vec));
    }
    jobs.extend(
        (0..shapes.len())
            .filter(|&i| !shapes[i].legal)
            .map(|i| vec![i]),
    );
    let single = |i: usize| {
        let src = program(shapes[i].ty, &[(i, &shapes[i])]);
        let (ok, text) = rig.check(&src, &format!("c{i}"));
        let _ = std::fs::remove_file(rig.dir.join(format!("c{i}.vyrn")));
        (i, src, ok, text)
    };
    let done = for_each_parallel(&jobs, |_, job| {
        if job.len() > 1 {
            let members: Vec<(usize, &Shape)> = job.iter().map(|&i| (i, &shapes[i])).collect();
            let tag = format!("k{}", job[0]);
            let (ok, _) = rig.check(&program(members[0].1.ty, &members), &tag);
            let _ = std::fs::remove_file(rig.dir.join(format!("{tag}.vyrn")));
            if ok {
                return job
                    .iter()
                    .map(|&i| {
                        (
                            i,
                            program(shapes[i].ty, &[(i, &shapes[i])]),
                            true,
                            String::new(),
                        )
                    })
                    .collect::<Vec<_>>();
            }
        }
        job.iter().map(|&i| single(i)).collect()
    });
    let mut checked: Vec<(String, bool, String)> = vec![Default::default(); shapes.len()];
    for (i, src, ok, text) in done.into_iter().flatten() {
        checked[i] = (src, ok, text);
    }
    let mut outcomes: Vec<Outcome> = vec![Outcome::default(); shapes.len()];
    let mut accepted: Vec<usize> = Vec::new();
    let mut hash = 0xcbf29ce484222325u64;
    for (i, (src, ok, text)) in checked.iter().enumerate() {
        fnv(&mut hash, &shapes[i].id);
        fnv(&mut hash, src);
        if *ok {
            accepted.push(i);
            continue;
        }
        let s = sentence(text);
        if s.contains("internal error") || s.contains("panicked") || s.is_empty() {
            outcomes[i].kinds.push("refused-badly".into());
            outcomes[i].detail = text.clone();
        } else if shapes[i].legal {
            outcomes[i].kinds.push("refused".into());
            outcomes[i].detail = text.clone();
        }
        outcomes[i].refusal = Some(s);
    }

    eprintln!("shapesweep: checked in {:.0?}", started.elapsed());
    // Batches of accepted shapes, by element type, in id order.
    let mut batches: Vec<Vec<usize>> = Vec::new();
    for ty in TYS {
        let mine: Vec<usize> = accepted
            .iter()
            .copied()
            .filter(|&i| shapes[i].ty == ty)
            .collect();
        for c in mine.chunks(BATCH) {
            batches.push(c.to_vec());
        }
    }
    let results = for_each_parallel(&batches, |_, batch| {
        let members: Vec<(usize, &Shape)> = batch.iter().map(|&i| (i, &shapes[i])).collect();
        let ty = members[0].1.ty;
        let src = program(ty, &members);
        let want = expected_out(&members);
        let tag = format!("b{}", members[0].0);
        let (ek, _) = rig.engine(&src, &tag, &want);
        let (rk, _) = rig.route(&src, &tag, &want);
        if ek.is_empty() && rk.is_empty() {
            return members
                .iter()
                .map(|(i, _)| (*i, Vec::new(), String::new()))
                .collect::<Vec<_>>();
        }
        // Split. The engine runs each shape alone; the route builds the
        // engine-clean shapes together once more, and each of the rest alone,
        // so a failure names one shape without a build per shape.
        let one = |i: usize, sh: &Shape| {
            let one = [(i, sh)];
            (program(sh.ty, &one), expected_out(&one))
        };
        let mut out: Vec<(usize, Vec<String>, String)> = Vec::new();
        for (i, sh) in &members {
            let (src, want) = one(*i, sh);
            let (kinds, detail) = rig.engine(&src, &format!("s{i}"), &want);
            out.push((*i, kinds, detail));
        }
        let clean: Vec<(usize, &Shape)> = members
            .iter()
            .zip(&out)
            .filter(|(_, o)| o.1.is_empty())
            .map(|(m, _)| *m)
            .collect();
        let together = if clean.is_empty() {
            true
        } else {
            let (src, want) = (program(ty, &clean), expected_out(&clean));
            rig.route(&src, &format!("t{}", clean[0].0), &want)
                .0
                .is_empty()
        };
        for (k, (i, sh)) in members.iter().enumerate() {
            if out[k].1.is_empty() && together {
                continue;
            }
            let (src, want) = one(*i, sh);
            let (kinds, detail) = rig.route(&src, &format!("r{i}"), &want);
            out[k].1.extend(kinds);
            out[k].2.push_str(&detail);
            out[k].1.sort();
            out[k].1.dedup();
        }
        out
    });
    let split = results
        .iter()
        .filter(|r| r.iter().any(|(_, k, _)| !k.is_empty()))
        .count();
    for r in results {
        for (i, kinds, detail) in r {
            outcomes[i].kinds = kinds;
            outcomes[i].detail = detail;
        }
    }

    // Verdicts.
    let mut failures: BTreeMap<String, String> = BTreeMap::new();
    let mut refusals: BTreeMap<String, usize> = BTreeMap::new();
    let (mut n_refused, mut n_ok) = (0usize, 0usize);
    for (i, sh) in shapes.iter().enumerate() {
        let o = &outcomes[i];
        fnv(&mut hash, &o.kinds.join("+"));
        if let Some(s) = &o.refusal {
            n_refused += 1;
            *refusals.entry(s.clone()).or_default() += 1;
        } else if o.kinds.is_empty() {
            n_ok += 1;
        }
        if !o.kinds.is_empty() {
            failures.insert(sh.id.clone(), o.kinds.join("+"));
            let program = if o.refusal.is_some() {
                checked[i].0.clone()
            } else {
                program(sh.ty, &[(i, sh)])
            };
            std::fs::create_dir_all(&out_dir).unwrap();
            std::fs::write(out_dir.join(format!("{}.vyrn", sh.id)), &program).unwrap();
            std::fs::write(
                out_dir.join(format!("{}.log", sh.id)),
                format!(
                    "{}\nwant: {}\n\n{}",
                    o.kinds.join("+"),
                    expected(sh),
                    o.detail
                ),
            )
            .unwrap();
        }
    }

    eprintln!(
        "shapesweep: {} generated, {} refused, {} accepted ({} clean, {} failing), {} batch(es) split, program-set hash {hash:016x}, {:.0?}",
        shapes.len(),
        n_refused,
        accepted.len(),
        n_ok,
        accepted.len() - n_ok,
        split,
        started.elapsed()
    );
    eprintln!(
        "shapesweep: cpu seconds: engine {}, route {}",
        rig.engine_us.load(Ordering::Relaxed) / 1_000_000,
        rig.route_us.load(Ordering::Relaxed) / 1_000_000
    );
    eprintln!("shapesweep: refusal sentences:");
    let mut by: Vec<_> = refusals.iter().collect();
    by.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    for (s, n) in by {
        eprintln!("  {n:>5}  {s}");
    }
    let mut kinds: BTreeMap<&str, usize> = BTreeMap::new();
    for k in failures.values() {
        *kinds.entry(k.as_str()).or_default() += 1;
    }
    eprintln!("shapesweep: {} failing; by kind: {kinds:?}", failures.len());
    if !failures.is_empty() {
        eprintln!(
            "shapesweep: failing programs and logs are in {}",
            out_dir.display()
        );
    }

    if only.is_some() {
        for (id, k) in &failures {
            eprintln!("  FAIL {id}  {k}");
        }
        return;
    }

    // The ratchet.
    let known = known();
    let fresh: Vec<(&String, &String)> = failures
        .iter()
        .filter(|(id, k)| known.get(*id) != Some(*k))
        .collect();
    let stale: Vec<&String> = known
        .keys()
        .filter(|id| !failures.contains_key(*id))
        .collect();
    if pin_write() {
        assert!(
            fresh.is_empty() || known.is_empty(),
            "the known list only shrinks; grow it by hand, with a reason:
{}",
            fresh
                .iter()
                .map(|(id, k)| format!("  {id}	{k}"))
                .collect::<Vec<_>>()
                .join(
                    "
"
                )
        );
        let text = std::fs::read_to_string(known_path()).unwrap_or_default();
        let mut keep: Vec<String> = text
            .lines()
            .filter(|l| l.starts_with('#') || known_row_alive(l, &failures))
            .map(str::to_string)
            .collect();
        if known.is_empty() {
            keep.extend(failures.iter().map(|(id, k)| format!("{id}	{k}")));
        }
        std::fs::write(
            known_path(),
            keep.join(
                "
",
            ) + "
",
        )
        .unwrap();
        return;
    }
    if !stale.is_empty() {
        eprintln!(
            "ratchet: {} known row(s) no longer fail; delete them with VYRN_PIN=write:",
            stale.len()
        );
        for s in &stale {
            eprintln!("  {s}");
        }
    }
    for (id, k) in fresh.iter().take(10) {
        let src = std::fs::read_to_string(out_dir.join(format!("{id}.vyrn"))).unwrap_or_default();
        let log = std::fs::read_to_string(out_dir.join(format!("{id}.log"))).unwrap_or_default();
        eprintln!(
            "
FAIL {id} ({k})
{log}
--- source ---
{src}"
        );
    }
    assert!(
        fresh.is_empty(),
        "{} shape(s) fail and the known list does not name them (programs in {}):
{}",
        fresh.len(),
        out_dir.display(),
        fresh
            .iter()
            .map(|(id, k)| format!("  {id}	{k}"))
            .collect::<Vec<_>>()
            .join(
                "
"
            )
    );
}

fn known_row_alive(line: &str, failures: &BTreeMap<String, String>) -> bool {
    line.split_once('\t')
        .is_some_and(|(id, k)| failures.get(id).is_some_and(|f| f == k))
}
