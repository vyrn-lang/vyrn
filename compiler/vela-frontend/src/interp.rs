//! A tree-walking interpreter for the v0.1 subset.
//!
//! This exists so Vela programs actually *run* today, with no LLVM. It is also
//! the executable reference semantics that the codegen backends must match.
//!
//! Control flow uses [`Ctrl`] in the error channel: a real error, or a
//! `?`-propagated early return of the whole function. This lets the `?` operator
//! (RFC-0005) short-circuit out of the middle of an expression.

use std::cell::RefCell;
use std::collections::HashMap;
use std::io::Write as _;

use crate::ast::*;

/// A runtime value.
#[derive(Debug, Clone, PartialEq)]
pub enum Val {
    Int(i64),
    /// A sized integer (`Int8`/`Int16`/`Int32`). `v` is the logical value,
    /// sign-extended into `i64`; arithmetic wraps back to `bits`.
    IntN { v: i64, bits: u8, signed: bool },
    /// A 64-bit float (`Float64`).
    Float(f64),
    /// A 32-bit float (`Float32`). Stored as `f32` so arithmetic rounds to single
    /// precision at each step, matching the native backend's `float` ops.
    Float32(f32),
    Bool(bool),
    Str(String),
    Unit,
    /// An optional (RFC-0005): `Some(v)` or `None`.
    Option(Option<Box<Val>>),
    /// A result (RFC-0005): `(is_ok, payload)` — `Ok(v)` is `(true, v)`.
    Result(bool, Box<Val>),
    /// A structural record (RFC-0002): field name -> value.
    Record(HashMap<String, Val>),
    /// A user-enum value (RFC-0002 §4): variant name + payload values.
    Enum(String, Vec<Val>),
    /// A generational reference (RFC-0004 §4, Path B): a slab slot index plus
    /// the generation captured when the reference was made. Access checks it
    /// against the slot's current generation.
    Ref { slot: usize, gen: u64 },
    /// A growable array (`Vec`). Used linearly; `push` returns a new value.
    Array(Vec<Val>),
}

/// One slot in the interpreter's cell slab: a generation and the boxed value.
#[derive(Debug, Clone)]
struct CellSlot {
    gen: u64,
    val: Val,
}

/// A control signal carried in the error channel.
pub enum Ctrl {
    /// A genuine runtime error.
    Err(String),
    /// A `?`-propagated early return of the enclosing function.
    Return(Val),
}

impl From<String> for Ctrl {
    fn from(s: String) -> Self {
        Ctrl::Err(s)
    }
}
impl From<&str> for Ctrl {
    fn from(s: &str) -> Self {
        Ctrl::Err(s.to_string())
    }
}

/// Statement/block control flow (distinct from the `Ctrl` error channel).
enum Flow {
    Normal,
    Return(Val),
}

/// Wrap `v` into a `bits`-wide two's-complement integer, matching the native
/// backend's `iN` arithmetic. Signed values are sign-extended back into `i64`;
/// unsigned are zero-extended. `bits >= 64` is the identity.
fn wrap_intn(v: i64, bits: u8, signed: bool) -> i64 {
    if bits >= 64 {
        return v;
    }
    let mask = (1i64 << bits) - 1;
    let m = v & mask;
    if signed && (m & (1i64 << (bits - 1))) != 0 {
        m | !mask // set the high bits (sign extension)
    } else {
        m
    }
}

/// Convert a numeric value to `target` (Int / sized IntN / Float / Float32),
/// matching the native casts (sext/trunc via `wrap_intn`, si/uitofp, fpto si/ui,
/// fp trunc/ext). Float→int truncates toward zero; out-of-range float→int is
/// unspecified (as in C/LLVM).
fn convert_val(v: Val, target: &Type) -> Val {
    match target {
        Type::Int => match v {
            Val::IntN { v, .. } => Val::Int(v),
            Val::Float(f) => Val::Int(f as i64),
            Val::Float32(f) => Val::Int(f as i64),
            other => other,
        },
        Type::IntN { bits, signed } => {
            let n = match v {
                Val::Int(n) => n,
                Val::IntN { v, .. } => v,
                // Truncate toward zero; an unsigned target reads the float as
                // `u64` (native `fptoui`), signed as `i64` (`fptosi`).
                Val::Float(f) if !*signed => f as u64 as i64,
                Val::Float(f) => f as i64,
                Val::Float32(f) if !*signed => f as u64 as i64,
                Val::Float32(f) => f as i64,
                other => return other,
            };
            Val::IntN { v: wrap_intn(n, *bits, *signed), bits: *bits, signed: *signed }
        }
        Type::Float => match v {
            Val::Int(n) => Val::Float(n as f64),
            // An unsigned source reads its bits as `u64` before converting
            // (native uses `uitofp`); signed sign-extends via `as f64`.
            Val::IntN { v, signed: false, .. } => Val::Float(v as u64 as f64),
            Val::IntN { v, signed: true, .. } => Val::Float(v as f64),
            Val::Float32(f) => Val::Float(f as f64), // fpext
            other => other,
        },
        // Float32 rounds every source to single precision (`as f32`).
        Type::Float32 => match v {
            Val::Int(n) => Val::Float32(n as f32),
            Val::IntN { v, signed: false, .. } => Val::Float32(v as u64 as f32),
            Val::IntN { v, signed: true, .. } => Val::Float32(v as f32),
            Val::Float(f) => Val::Float32(f as f32), // fptrunc
            other => other,
        },
        _ => v,
    }
}

// ---- text encodings (hex / base64 / url) --------------------------------
// Hand-rolled so the algorithm is identical to the native runtime (parity).
// Encoders take a string's UTF-8 bytes → ASCII text. Decoders parse back to
// bytes, then require the result to be valid UTF-8 (else `None`).

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn hex_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 2);
    for b in s.bytes() {
        out.push(char::from_digit((b >> 4) as u32, 16).unwrap());
        out.push(char::from_digit((b & 0xf) as u32, 16).unwrap());
    }
    out
}

fn hex_val(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

fn hex_decode(s: &str) -> Option<String> {
    let b = s.as_bytes();
    if b.len() % 2 != 0 {
        return None;
    }
    let mut bytes = Vec::with_capacity(b.len() / 2);
    let mut i = 0;
    while i < b.len() {
        bytes.push((hex_val(b[i])? << 4) | hex_val(b[i + 1])?);
        i += 2;
    }
    String::from_utf8(bytes).ok()
}

fn base64_encode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = String::new();
    let mut i = 0;
    while i + 3 <= b.len() {
        let n = ((b[i] as u32) << 16) | ((b[i + 1] as u32) << 8) | (b[i + 2] as u32);
        out.push(B64[(n >> 18) as usize & 63] as char);
        out.push(B64[(n >> 12) as usize & 63] as char);
        out.push(B64[(n >> 6) as usize & 63] as char);
        out.push(B64[n as usize & 63] as char);
        i += 3;
    }
    let rem = b.len() - i;
    if rem == 1 {
        let n = (b[i] as u32) << 16;
        out.push(B64[(n >> 18) as usize & 63] as char);
        out.push(B64[(n >> 12) as usize & 63] as char);
        out.push('=');
        out.push('=');
    } else if rem == 2 {
        let n = ((b[i] as u32) << 16) | ((b[i + 1] as u32) << 8);
        out.push(B64[(n >> 18) as usize & 63] as char);
        out.push(B64[(n >> 12) as usize & 63] as char);
        out.push(B64[(n >> 6) as usize & 63] as char);
        out.push('=');
    }
    out
}

fn b64_val(c: u8) -> Option<u8> {
    match c {
        b'A'..=b'Z' => Some(c - b'A'),
        b'a'..=b'z' => Some(c - b'a' + 26),
        b'0'..=b'9' => Some(c - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

fn base64_decode(s: &str) -> Option<String> {
    let b = s.as_bytes();
    if b.len() % 4 != 0 {
        return None;
    }
    let mut bytes = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let c0 = b64_val(b[i])?;
        let c1 = b64_val(b[i + 1])?;
        // The last group may carry one or two `=` pad characters.
        let p2 = b[i + 2] == b'=';
        let p3 = b[i + 3] == b'=';
        let c2 = if p2 { 0 } else { b64_val(b[i + 2])? };
        let c3 = if p3 { 0 } else { b64_val(b[i + 3])? };
        // Padding is only legal in the final group, and `=X` (pad then data) is not.
        if (p2 || p3) && i + 4 != b.len() {
            return None;
        }
        if p2 && !p3 {
            return None;
        }
        let n = ((c0 as u32) << 18) | ((c1 as u32) << 12) | ((c2 as u32) << 6) | c3 as u32;
        bytes.push((n >> 16) as u8);
        if !p2 {
            bytes.push((n >> 8) as u8);
        }
        if !p3 {
            bytes.push(n as u8);
        }
        i += 4;
    }
    String::from_utf8(bytes).ok()
}

/// Unreserved URL characters (RFC 3986): everything else is percent-encoded.
fn url_unreserved(c: u8) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.' | b'~')
}

fn url_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if url_unreserved(b) {
            out.push(b as char);
        } else {
            out.push('%');
            out.push(char::from_digit((b >> 4) as u32, 16).unwrap().to_ascii_uppercase());
            out.push(char::from_digit((b & 0xf) as u32, 16).unwrap().to_ascii_uppercase());
        }
    }
    out
}

fn url_decode(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut bytes = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            if i + 2 >= b.len() {
                return None;
            }
            bytes.push((hex_val(b[i + 1])? << 4) | hex_val(b[i + 2])?);
            i += 3;
        } else {
            bytes.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(bytes).ok()
}

/// Parse a base-10 integer with strict, backend-matched semantics: an optional
/// leading `-`, then one or more ASCII digits, and nothing else (no whitespace,
/// no `+`). Returns `None` on any deviation. Overflow *wraps* (it is not
/// rejected) so the result matches the native backend bit-for-bit.
fn parse_int(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.is_empty() {
        return None;
    }
    let (neg, start) = if b[0] == b'-' { (true, 1) } else { (false, 0) };
    if start == b.len() {
        return None; // just "-"
    }
    let mut n: i64 = 0;
    for &c in &b[start..] {
        if !c.is_ascii_digit() {
            return None;
        }
        n = n.wrapping_mul(10).wrapping_add((c - b'0') as i64);
    }
    Some(if neg { n.wrapping_neg() } else { n })
}

/// Run the program's `main` and return its integer result.
///
/// The tree-walking interpreter recurses once per Vela call, so a deeply
/// recursive program can exhaust the OS main-thread stack (only ~1 MB on
/// Windows). Run the interpreter on a dedicated thread with a large stack so
/// recursion depth is bounded by the program, not the platform default.
pub fn run(program: &Program) -> Result<i64, String> {
    std::thread::scope(|s| {
        std::thread::Builder::new()
            .stack_size(256 * 1024 * 1024)
            .spawn_scoped(s, || run_inner(program))
            .expect("failed to spawn interpreter thread")
            .join()
            .unwrap_or_else(|_| Err("interpreter thread panicked (likely stack overflow)".into()))
    })
}

fn run_inner(program: &Program) -> Result<i64, String> {
    // The same ownership analysis the native backend uses to reclaim heap
    // values at block exit. Freeing a string/array buffer is invisible from
    // inside the language, but auto-*releasing* a reference cell is not: the
    // slot returns to the slab (a million loop iterations fit in 65536 cells)
    // and any illegally retained alias must trap. The interpreter executes the
    // identical plan so both backends observe the same slab behavior.
    // Identities are `Stmt` node addresses — unique program-wide, so the
    // per-function maps flatten into one.
    let ownership = crate::own::analyze(program);
    let droppable: HashMap<usize, crate::own::DropKind> =
        ownership.droppable.into_values().flatten().collect();
    let funcs: HashMap<&str, &Function> =
        program.functions.iter().map(|f| (f.name.as_str(), f)).collect();
    let types: HashMap<&str, &TypeDecl> =
        program.type_decls.iter().map(|t| (t.name.as_str(), t)).collect();
    // Enum variant names, so constructor uses (Var/Call) can be recognized.
    let mut variants: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for t in &program.type_decls {
        if let Type::Enum(vs) = &t.base {
            for v in vs {
                variants.insert(v.name.as_str());
            }
        }
    }
    // Open the log file up front if the program directs logs to one.
    let log_file = match &program.log_sink {
        LogSink::File(path) => {
            let f = std::fs::File::create(path)
                .map_err(|e| format!("cannot open log file `{path}`: {e}"))?;
            RefCell::new(Some(f))
        }
        _ => RefCell::new(None),
    };
    let interp = Interp {
        funcs,
        types,
        variants,
        droppable,
        cells: RefCell::new(Vec::new()),
        free: RefCell::new(Vec::new()),
        log_level: program.log_level,
        log_sink: program.log_sink.clone(),
        log_file,
        protocol_methods: program
            .protocols
            .iter()
            .flat_map(|p| p.methods.iter().map(|m| (m.name.clone(), p.name.clone())))
            .collect(),
        variant_enum: program
            .type_decls
            .iter()
            .filter_map(|d| match &d.base {
                Type::Enum(vs) => Some(vs.iter().map(|v| (v.name.clone(), d.name.clone()))),
                _ => None,
            })
            .flatten()
            .collect(),
        region_depth: std::cell::Cell::new(0),
    };
    match interp.call("main", &[]) {
        Ok(Val::Int(n)) => Ok(n),
        Ok(other) => Err(format!("main returned {other:?}, expected Int64")),
        Err(Ctrl::Err(s)) => Err(s),
        Err(Ctrl::Return(_)) => Err("internal: `?` propagated past main".into()),
    }
}

struct Interp<'a> {
    funcs: HashMap<&'a str, &'a Function>,
    types: HashMap<&'a str, &'a TypeDecl>,
    variants: std::collections::HashSet<&'a str>,
    /// Droppable `let` bindings (by `Stmt` node address) and their reclamation
    /// kind — the ownership analysis shared with the native backend.
    droppable: HashMap<usize, crate::own::DropKind>,
    /// The generational-reference cell slab (RFC-0004 §4, Path B).
    cells: RefCell<Vec<CellSlot>>,
    /// Free slots available for reuse (their generation was already bumped).
    free: RefCell<Vec<usize>>,
    /// The logging threshold ordinal (RFC-0008); calls below it are skipped.
    log_level: usize,
    /// Where log records are written (RFC-0008).
    log_sink: LogSink,
    /// The open log file, when `log_sink` is [`LogSink::File`].
    log_file: RefCell<Option<std::fs::File>>,
    /// Protocol methods (RFC-0002 §5): method name -> protocol name.
    protocol_methods: HashMap<String, String>,
    /// Enum variant name -> its enum's name, for dispatching on enum receivers.
    variant_enum: HashMap<String, String>,
    /// Current `region { .. }` nesting depth. The native runtime runs regions
    /// on a fixed 64-slot arena stack and traps past it; the interpreter
    /// enforces the same bound so the two stay observably identical.
    region_depth: std::cell::Cell<usize>,
}

/// A scope binding: the current value plus the declared type, when one exists
/// (a `let` annotation or a function parameter). The type is what a later
/// assignment must coerce — and therefore auto-validate — back into, mirroring
/// the native backend's typed stores.
#[derive(Clone)]
struct Slot {
    v: Val,
    ty: Option<Type>,
}

impl Slot {
    fn untyped(v: Val) -> Slot {
        Slot { v, ty: None }
    }
}

impl<'a> Interp<'a> {
    fn call(&self, name: &str, args: &[Val]) -> Result<Val, Ctrl> {
        Ok(self.call_capturing(name, args)?.0)
    }

    /// Like [`call`], but also returns the final values of the parameters (so the
    /// caller can copy `modify` parameters back — call-by-value-result).
    fn call_capturing(&self, name: &str, args: &[Val]) -> Result<(Val, Vec<Val>), Ctrl> {
        let f = self
            .funcs
            .get(name)
            .ok_or_else(|| Ctrl::Err(format!("call to unknown function `{name}`")))?;
        // An `extern` (RFC-0012) is host-provided; the interpreter has no host to
        // call, so a *call* traps with the canonical wording (byte-identical to
        // the native backend's inline trap). Declaring one is fine — only calling
        // it here is the effect the interpreter cannot honor.
        if f.is_extern {
            return Err(Ctrl::Err(format!(
                "extern `{name}` is not available on this target"
            )));
        }
        let mut scope: Vec<HashMap<String, Slot>> = vec![HashMap::new()];
        for (p, v) in f.params.iter().zip(args) {
            // Coerce each argument to its parameter type (sized-int wrapping,
            // and automatic validation into predicated types).
            let coerced = self.coerce(v.clone(), &p.ty)?;
            scope[0].insert(p.name.clone(), Slot { v: coerced, ty: Some(p.ty.clone()) });
        }
        // A `?` inside the body surfaces as Ctrl::Return; catch it as the result.
        let ret = match self.block(&f.body, &mut scope) {
            Ok(Flow::Return(v)) => v,
            Ok(Flow::Normal) => Val::Unit,
            Err(Ctrl::Return(v)) => v,
            Err(e) => return Err(e),
        };
        // Coerce the return value to the declared return type.
        let ret = self.coerce(ret, &f.ret)?;
        let finals = f
            .params
            .iter()
            .map(|p| scope[0].get(&p.name).map(|s| s.v.clone()).unwrap_or(Val::Unit))
            .collect();
        Ok((ret, finals))
    }

    /// Construct a validated-type value: evaluate the refinement predicate on
    /// `v` and fail if it does not hold. The runtime representation of a
    /// validated value is just its base value (zero overhead).
    fn construct(&self, decl: &TypeDecl, v: Val) -> Result<Val, Ctrl> {
        if !self.validates(decl, &v)? {
            return Err(format!("validation failed for `{}`", decl.name).into());
        }
        Ok(v)
    }

    fn block(&self, block: &Block, scope: &mut Vec<HashMap<String, Slot>>) -> Result<Flow, Ctrl> {
        scope.push(HashMap::new());
        // Values reclaimed when this frame exits (normally or via `return`),
        // mirroring the native backend's block-exit drops. Only a reference
        // release is observable here (the slab slot is recycled and stale
        // aliases must trap); string/array buffers are host-reclaimed. The
        // value is captured at the `let` (droppable bindings are immutable),
        // which also keeps shadowed bindings reclaimable.
        let mut drops: Vec<Val> = Vec::new();
        for stmt in &block.stmts {
            let flow = self.stmt(stmt, scope);
            match flow {
                Ok(Flow::Return(v)) => {
                    self.run_drops(&drops)?;
                    scope.pop();
                    return Ok(Flow::Return(v));
                }
                Ok(Flow::Normal) => {
                    if let Stmt::Let { name, .. } = stmt {
                        if let Some(kind) = self.droppable.get(&(stmt as *const Stmt as usize)) {
                            if *kind == crate::own::DropKind::ReleaseRef {
                                if let Some(slot) = scope.last().unwrap().get(name) {
                                    drops.push(slot.v.clone());
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    scope.pop();
                    return Err(e);
                }
            }
        }
        let r = self.run_drops(&drops);
        scope.pop();
        r?;
        Ok(Flow::Normal)
    }

    /// Execute a frame's pending block-exit drops: release each captured
    /// reference (bumping its slot's generation, exactly like the emitted
    /// `release` in the native backend).
    fn run_drops(&self, drops: &[Val]) -> Result<(), Ctrl> {
        for v in drops {
            if let Val::Ref { slot, gen } = v {
                self.cell_release(*slot, *gen)?;
            }
        }
        Ok(())
    }

    fn stmt(&self, stmt: &Stmt, scope: &mut Vec<HashMap<String, Slot>>) -> Result<Flow, Ctrl> {
        match stmt {
            Stmt::Let { name, value, ty, .. } => {
                let mut v = self.expr(value, scope)?;
                // An annotation coerces the initializer (sized-int wrapping,
                // automatic validation) and is remembered so reassignments run
                // through the same coercion.
                if let Some(t) = ty {
                    v = self.coerce(v, t)?;
                }
                scope.last_mut().unwrap().insert(name.clone(), Slot { v, ty: ty.clone() });
                Ok(Flow::Normal)
            }
            Stmt::Assign { name, value, .. } => {
                let v = self.expr(value, scope)?;
                // Reassignment flows through the binding's declared type — the
                // same coercion (and automatic validation) as the original let.
                let declared =
                    scope.iter().rev().find_map(|f| f.get(name).and_then(|s| s.ty.clone()));
                let v = match &declared {
                    Some(t) => self.coerce(v, t)?,
                    None => v,
                };
                for frame in scope.iter_mut().rev() {
                    if let Some(slot) = frame.get_mut(name) {
                        slot.v = v;
                        return Ok(Flow::Normal);
                    }
                }
                Err(format!("assignment to unbound variable `{name}`").into())
            }
            Stmt::SetField { name, field, value, .. } => {
                let v = self.expr(value, scope)?;
                for frame in scope.iter_mut().rev() {
                    if let Some(Slot { v: Val::Record(map), .. }) = frame.get_mut(name) {
                        map.insert(field.clone(), v);
                        return Ok(Flow::Normal);
                    }
                }
                Err(format!("field assignment to unbound record `{name}`").into())
            }
            // `name[index] = value` — in-place element store (RFC-0011). The
            // value coerces into the declared element type (sized-int wrapping,
            // automatic validation), then is written through the shared buffer;
            // an out-of-bounds index traps with the read path's wording.
            Stmt::IndexSet { name, index, value, .. } => {
                let iv = self.expr(index, scope)?;
                let idx = match iv {
                    Val::Int(n) => n,
                    other => return Err(format!("array index must be an Int64, found {other:?}").into()),
                };
                let mut v = self.expr(value, scope)?;
                // Coerce into the element type of the array binding's declared
                // type (validated element types validate here, exactly like a
                // `push` argument or an annotated `let`).
                let elem_ty = scope.iter().rev().find_map(|f| {
                    f.get(name).and_then(|s| match &s.ty {
                        Some(Type::Array(t)) | Some(Type::ArrayN(t, _)) => Some((**t).clone()),
                        _ => None,
                    })
                });
                if let Some(t) = &elem_ty {
                    v = self.coerce(v, t)?;
                }
                for frame in scope.iter_mut().rev() {
                    if let Some(Slot { v: Val::Array(items), .. }) = frame.get_mut(name) {
                        if idx < 0 || idx as usize >= items.len() {
                            return Err(format!("array index {idx} out of bounds").into());
                        }
                        items[idx as usize] = v;
                        return Ok(Flow::Normal);
                    }
                }
                Err(format!("index-assignment to unbound array `{name}`").into())
            }
            Stmt::Return { value, .. } => {
                let v = match value {
                    Some(e) => self.expr(e, scope)?,
                    None => Val::Unit,
                };
                Ok(Flow::Return(v))
            }
            Stmt::If { cond, then_block, else_block, .. } => {
                if self.as_bool(self.expr(cond, scope)?)? {
                    self.block(then_block, scope)
                } else if let Some(eb) = else_block {
                    self.block(eb, scope)
                } else {
                    Ok(Flow::Normal)
                }
            }
            Stmt::While { cond, body, .. } => {
                while self.as_bool(self.expr(cond, scope)?)? {
                    if let Flow::Return(v) = self.block(body, scope)? {
                        return Ok(Flow::Return(v));
                    }
                }
                Ok(Flow::Normal)
            }
            Stmt::ForIn { var, iter, body, .. } => {
                let items = match self.expr(iter, scope)? {
                    Val::Array(items) => items,
                    // Iterating a String yields each byte as an Int.
                    Val::Str(s) => s.as_bytes().iter().map(|b| Val::Int(*b as i64)).collect(),
                    other => return Err(format!("`for` expected an array, found {other:?}").into()),
                };
                for item in items {
                    // Fresh frame per iteration holding the loop variable; the
                    // body's own inner frame nests inside it.
                    scope.push(HashMap::new());
                    scope.last_mut().unwrap().insert(var.clone(), Slot::untyped(item));
                    let flow = self.block(body, scope);
                    scope.pop();
                    if let Flow::Return(v) = flow? {
                        return Ok(Flow::Return(v));
                    }
                }
                Ok(Flow::Normal)
            }
            Stmt::Drop { name, .. } => {
                // A reference is released — its slot's generation bumps, so any
                // later (illegally aliased) use traps, matching the native
                // backend. Strings and arrays are reclaimed by the host, which is
                // not observable, so dropping them has no runtime effect here.
                let v = scope.iter().rev().find_map(|f| f.get(name)).map(|s| s.v.clone());
                if let Some(Val::Ref { slot, gen }) = v {
                    self.cell_release(slot, gen)?;
                }
                Ok(Flow::Normal)
            }
            Stmt::Expr(e) => {
                self.expr(e, scope)?;
                Ok(Flow::Normal)
            }
            // A `region` is semantically transparent to the reference
            // interpreter — it runs its body in a fresh scope and the host
            // reclaims memory. Deterministic freeing is observable only in the
            // native backend; the two agree on output and exit code.
            Stmt::Region { body, .. } => {
                // Match the native arena runtime's fixed 64-slot region stack:
                // entering a 65th nested region traps there, so trap here with
                // the same message (interp == native, incl. traps).
                if self.region_depth.get() >= 64 {
                    return Err("region nesting exceeds 64".into());
                }
                self.region_depth.set(self.region_depth.get() + 1);
                let r = self.block(body, scope);
                self.region_depth.set(self.region_depth.get() - 1);
                r
            }
        }
    }

    fn expr(&self, expr: &Expr, scope: &mut Vec<HashMap<String, Slot>>) -> Result<Val, Ctrl> {
        match expr {
            Expr::Int(n) => Ok(Val::Int(*n)),
            Expr::Float(x) => Ok(Val::Float(*x)),
            Expr::Bool(b) => Ok(Val::Bool(*b)),
            Expr::Str(s) => Ok(Val::Str(s.clone())),
            Expr::Var { name, .. } => {
                // `None` is the empty-Option constructor, not a variable.
                if name == "None" {
                    return Ok(Val::Option(None));
                }
                // A nullary enum variant, e.g. `Empty`.
                if self.variants.contains(name.as_str()) {
                    return Ok(Val::Enum(name.clone(), Vec::new()));
                }
                for frame in scope.iter().rev() {
                    if let Some(slot) = frame.get(name) {
                        return Ok(slot.v.clone());
                    }
                }
                Err(format!("unbound variable `{name}`").into())
            }
            Expr::Unary { op, expr, .. } => {
                let v = self.expr(expr, scope)?;
                match (op, v) {
                    // wrapping: -i64::MIN has no representation; two's complement
                    // keeps it MIN, exactly as native `sub i64 0, %n` does.
                    (UnOp::Neg, Val::Int(n)) => Ok(Val::Int(n.wrapping_neg())),
                    (UnOp::Neg, Val::IntN { v, bits, signed }) => {
                        Ok(Val::IntN { v: wrap_intn(v.wrapping_neg(), bits, signed), bits, signed })
                    }
                    (UnOp::Neg, Val::Float(x)) => Ok(Val::Float(-x)),
                    (UnOp::Neg, Val::Float32(x)) => Ok(Val::Float32(-x)),
                    (UnOp::Not, Val::Bool(b)) => Ok(Val::Bool(!b)),
                    _ => Err("type error in unary op (should have been caught)".into()),
                }
            }
            Expr::Binary { op, lhs, rhs, .. } => {
                if let BinOp::And = op {
                    return Ok(Val::Bool(
                        self.as_bool(self.expr(lhs, scope)?)?
                            && self.as_bool(self.expr(rhs, scope)?)?,
                    ));
                }
                if let BinOp::Or = op {
                    return Ok(Val::Bool(
                        self.as_bool(self.expr(lhs, scope)?)?
                            || self.as_bool(self.expr(rhs, scope)?)?,
                    ));
                }
                let l = self.expr(lhs, scope)?;
                let r = self.expr(rhs, scope)?;
                self.binop(*op, l, r)
            }
            Expr::Call { name, args, .. } => {
                // `schemaOf(TypeName)` reflects a type at compile time — its
                // argument is a type name, not a value — so build and evaluate its
                // `Schema` literal before the normal argument evaluation.
                if name == "schemaOf" {
                    if let Some(Expr::Var { name: tn, .. }) = args.first() {
                        if let Some(decl) = self.types.get(tn.as_str()) {
                            let sl = crate::types::schema_struct_lit(decl);
                            return self.expr(&sl, scope);
                        }
                    }
                    return Err("`schemaOf` needs a declared type name".into());
                }
                // `jsonSchema(TypeName)` renders the declared type as a JSON Schema
                // string at compile time — computed from the declaration, so both
                // backends produce identical bytes.
                if name == "jsonSchema" {
                    if let Some(Expr::Var { name: tn, .. }) = args.first() {
                        if self.types.contains_key(tn.as_str()) {
                            // `json_schema_string` wants an owned `TypeDecl` map; the
                            // interpreter keeps borrows, so materialize one here (only
                            // on this rare compile-time-reflection call).
                            let owned: std::collections::HashMap<String, crate::ast::TypeDecl> =
                                self.types.iter().map(|(k, v)| (k.to_string(), (*v).clone())).collect();
                            let js = crate::types::json_schema_string(&owned[tn.as_str()], &owned);
                            return Ok(Val::Str(js));
                        }
                    }
                    return Err("`jsonSchema` needs a declared type name".into());
                }
                // `a.pop()` (RFC-0011) — remove and return the last element as
                // `Option<T>` (`None` on empty), mutating the receiver in place.
                // Handled before the generic argument evaluation because it needs
                // to write the shrunk array back through the binding.
                if name == "@pop" {
                    if let Some(Expr::Var { name: recv, .. }) = args.first() {
                        for frame in scope.iter_mut().rev() {
                            if let Some(Slot { v: Val::Array(items), .. }) = frame.get_mut(recv) {
                                let popped = items.pop();
                                return Ok(Val::Option(popped.map(Box::new)));
                            }
                        }
                    }
                    return Err("`pop` needs a mutable array binding".into());
                }
                // `a.swapRemove(i)` (RFC-0011) — move the last element into slot
                // `i`, shrink by one, return the old element `i`. Traps on an
                // out-of-bounds index with the read path's wording.
                if name == "@swapRemove" {
                    let Some(Expr::Var { name: recv, .. }) = args.first() else {
                        return Err("`swapRemove` needs a mutable array binding".into());
                    };
                    let recv = recv.clone();
                    let idx = match self.expr(&args[1], scope)? {
                        Val::Int(n) => n,
                        other => {
                            return Err(format!("array index must be an Int64, found {other:?}").into())
                        }
                    };
                    for frame in scope.iter_mut().rev() {
                        if let Some(Slot { v: Val::Array(items), .. }) = frame.get_mut(&recv) {
                            if idx < 0 || idx as usize >= items.len() {
                                return Err(format!("array index {idx} out of bounds").into());
                            }
                            return Ok(items.swap_remove(idx as usize));
                        }
                    }
                    return Err("`swapRemove` needs a mutable array binding".into());
                }
                let mut vals = Vec::with_capacity(args.len());
                for a in args {
                    vals.push(self.expr(a, scope)?);
                }
                // Numeric conversion `Int32(x)`, `Float64(x)`, ...
                if let Some(target) = crate::types::numeric_conv_target(name) {
                    if vals.len() == 1 {
                        return Ok(convert_val(vals.remove(0), &target));
                    }
                }
                match name.as_str() {
                    "print" => {
                        match &vals[0] {
                            Val::Int(n) => println!("{n}"),
                            // A sized int prints its logical value; unsigned
                            // formats the bits as `u64` (native uses %lu).
                            Val::IntN { v, signed: true, .. } => println!("{v}"),
                            Val::IntN { v, signed: false, .. } => println!("{}", *v as u64),
                            // Fixed 6-decimal precision matches native `printf("%f")`
                            // exactly (Rust's shortest-repr Display would not). A
                            // Float32 promotes to f64 for printing, as C varargs do.
                            Val::Float(x) => println!("{x:.6}"),
                            Val::Float32(x) => println!("{:.6}", *x as f64),
                            Val::Bool(b) => println!("{b}"),
                            Val::Str(s) => println!("{s}"),
                            other => println!("{other:?}"),
                        }
                        Ok(Val::Unit)
                    }
                    // A logger handle is its name string (RFC-0008).
                    "logger" => Ok(vals.remove(0)),
                    // Log methods write `[LEVEL] name: msg` to stderr (kept off
                    // stdout, so program output and logs are separable — the
                    // "where does it print" concern behind RFC-0008).
                    "trace" | "debug" | "info" | "warn" | "error" => {
                        // Drop calls below the configured threshold (RFC-0008).
                        if log_level_ordinal(name).unwrap_or(0) >= self.log_level {
                            let lname = match &vals[0] {
                                Val::Str(s) => s.clone(),
                                other => format!("{other:?}"),
                            };
                            let msg = match &vals[1] {
                                Val::Str(s) => s.clone(),
                                other => format!("{other:?}"),
                            };
                            let line = format!("[{}] {lname}: {msg}", name.to_uppercase());
                            match &self.log_sink {
                                LogSink::Stderr => eprintln!("{line}"),
                                LogSink::Stdout => println!("{line}"),
                                LogSink::File(_) => {
                                    if let Some(f) = self.log_file.borrow_mut().as_mut() {
                                        let _ = writeln!(f, "{line}");
                                    }
                                }
                            }
                        }
                        Ok(Val::Unit)
                    }
                    // `@concat` — internal spelling produced by interpolation
                    // (the surface form is `a + b`, handled in `binop`).
                    "@concat" => match (&vals[0], &vals[1]) {
                        (Val::Str(a), Val::Str(b)) => Ok(Val::Str(format!("{a}{b}"))),
                        _ => Err("@concat of non-Strings".into()),
                    },
                    "contains" => match (&vals[0], &vals[1]) {
                        (Val::Str(a), Val::Str(b)) => Ok(Val::Bool(a.contains(b.as_str()))),
                        _ => Err("contains of non-Strings".into()),
                    },
                    "startsWith" => match (&vals[0], &vals[1]) {
                        (Val::Str(a), Val::Str(b)) => Ok(Val::Bool(a.starts_with(b.as_str()))),
                        _ => Err("startsWith of non-Strings".into()),
                    },
                    "endsWith" => match (&vals[0], &vals[1]) {
                        (Val::Str(a), Val::Str(b)) => Ok(Val::Bool(a.ends_with(b.as_str()))),
                        _ => Err("endsWith of non-Strings".into()),
                    },
                    // `bytes` decodes the UTF-8 bytes; `chars` the code points.
                    "bytes" => match &vals[0] {
                        Val::Str(s) => {
                            Ok(Val::Array(s.bytes().map(|b| Val::Int(b as i64)).collect()))
                        }
                        _ => Err("bytes of non-String".into()),
                    },
                    "chars" => match &vals[0] {
                        Val::Str(s) => {
                            Ok(Val::Array(s.chars().map(|c| Val::Int(c as i64)).collect()))
                        }
                        _ => Err("chars of non-String".into()),
                    },
                    // Text encodings: encoders return a String; decoders return
                    // `Option<String>` (None on malformed input or non-UTF-8 result).
                    "hexEncode" => match &vals[0] {
                        Val::Str(s) => Ok(Val::Str(hex_encode(s))),
                        _ => Err("hexEncode of non-String".into()),
                    },
                    "base64Encode" => match &vals[0] {
                        Val::Str(s) => Ok(Val::Str(base64_encode(s))),
                        _ => Err("base64Encode of non-String".into()),
                    },
                    "urlEncode" => match &vals[0] {
                        Val::Str(s) => Ok(Val::Str(url_encode(s))),
                        _ => Err("urlEncode of non-String".into()),
                    },
                    "hexDecode" | "base64Decode" | "urlDecode" => {
                        let out = match &vals[0] {
                            Val::Str(s) => match name.as_str() {
                                "hexDecode" => hex_decode(s),
                                "base64Decode" => base64_decode(s),
                                _ => url_decode(s),
                            },
                            _ => return Err(format!("{name} of non-String").into()),
                        };
                        Ok(Val::Option(out.map(|s| Box::new(Val::Str(s)))))
                    }
                    // `@str` (from `x.toString()` and interpolation) must render
                    // exactly as `print` does: signed IntN by value, unsigned as
                    // `u64`, Float to 6 decimals.
                    "@str" => match &vals[0] {
                        Val::Int(n) => Ok(Val::Str(n.to_string())),
                        Val::IntN { v, signed: true, .. } => Ok(Val::Str(v.to_string())),
                        Val::IntN { v, signed: false, .. } => Ok(Val::Str((*v as u64).to_string())),
                        Val::Float(f) => Ok(Val::Str(format!("{f:.6}"))),
                        Val::Float32(f) => Ok(Val::Str(format!("{:.6}", *f as f64))),
                        Val::Bool(b) => Ok(Val::Str(if *b { "true" } else { "false" }.to_string())),
                        Val::Str(s) => Ok(Val::Str(s.clone())),
                        other => Err(format!("str of unsupported value {other:?}").into()),
                    },
                    "parse" => match &vals[0] {
                        Val::Str(s) => Ok(Val::Option(parse_int(s).map(|n| Box::new(Val::Int(n))))),
                        other => Err(format!("parse of non-String {other:?}").into()),
                    },
                    "cell" => self.cell_alloc(vals.remove(0)),
                    "get" => {
                        let (slot, gen) = self.as_ref(&vals[0])?;
                        self.cell_get(slot, gen)
                    }
                    "set" => {
                        let (slot, gen) = self.as_ref(&vals[0])?;
                        self.cell_set(slot, gen, vals[1].clone())?;
                        Ok(Val::Unit)
                    }
                    "release" => {
                        let (slot, gen) = self.as_ref(&vals[0])?;
                        self.cell_release(slot, gen)?;
                        Ok(Val::Unit)
                    }
                    "array" => Ok(Val::Array(Vec::new())),
                    "push" => match &vals[0] {
                        Val::Array(elems) => {
                            let mut next = elems.clone();
                            next.push(vals[1].clone());
                            Ok(Val::Array(next))
                        }
                        other => Err(format!("push of non-Array {other:?}").into()),
                    },
                    "at" => match (&vals[0], &vals[1]) {
                        (Val::Array(elems), Val::Int(i)) => elems
                            .get(*i as usize)
                            .cloned()
                            .ok_or_else(|| format!("array index {i} out of bounds").into()),
                        // `s[i]` on a String is the byte at index `i` (bounds-checked).
                        (Val::Str(s), Val::Int(i)) => s
                            .as_bytes()
                            .get(*i as usize)
                            .map(|b| Val::Int(*b as i64))
                            .ok_or_else(|| format!("string index {i} out of bounds").into()),
                        _ => Err("at of non-Array/Int64".into()),
                    },
                    "alen" => match &vals[0] {
                        Val::Array(elems) => Ok(Val::Int(elems.len() as i64)),
                        other => Err(format!("alen of non-Array {other:?}").into()),
                    },
                    // Reclamation is observable only in native code (the host
                    // frees the Vec); the two agree on output and exit code.
                    "afree" => match &vals[0] {
                        Val::Array(_) => Ok(Val::Unit),
                        other => Err(format!("afree of non-Array {other:?}").into()),
                    },
                    // value(x) -> Value: box a scalar into the interpolation enum.
                    "value" => {
                        let v = vals.remove(0);
                        let variant = match &v {
                            Val::Int(_) => "IntVal",
                            Val::Bool(_) => "BoolVal",
                            Val::Str(_) => "StrVal",
                            other => return Err(format!("value of {other:?}").into()),
                        };
                        Ok(Val::Enum(variant.to_string(), vec![v]))
                    }
                    // `@list` (tagged-template desugaring): fixed and growable
                    // arrays share a runtime representation here — the identity.
                    "@list" => match &vals[0] {
                        Val::Array(_) => Ok(vals.remove(0)),
                        other => Err(format!("@list of non-Array {other:?}").into()),
                    },
                    // `@join` (`t.join()`) awaits a task; eager tasks are in hand.
                    "@join" => Ok(vals.remove(0)),
                    "Some" => Ok(Val::Option(Some(Box::new(vals.remove(0))))),
                    "Ok" => Ok(Val::Result(true, Box::new(vals.remove(0)))),
                    "Err" => Ok(Val::Result(false, Box::new(vals.remove(0)))),
                    _ => {
                        // Protocol-method dispatch (RFC-0002 §5): resolve by the
                        // receiver's runtime type to the impl, then call it.
                        if let Some(proto) = self.protocol_methods.get(name.as_str()).cloned() {
                            let key = self.val_type_key(&vals[0]).ok_or_else(|| {
                                Ctrl::Err(format!("cannot dispatch `{name}` on {:?}", vals[0]))
                            })?;
                            let mangled = crate::types::impl_method_name(&proto, &key, name);
                            return self.call(&mangled, &vals);
                        }
                        // Enum variant with payload(s), e.g. `Circle(5)`, `Rect(w, h)`.
                        if self.variants.contains(name.as_str()) {
                            return Ok(Val::Enum(name.clone(), vals));
                        }
                        if let Some(decl) = self.types.get(name.as_str()) {
                            return self.construct(decl, vals.remove(0));
                        }
                        // `modify` parameters copy back into the caller's variable
                        // after the call (call-by-value-result).
                        let modifies: Vec<usize> = self
                            .funcs
                            .get(name.as_str())
                            .map(|f| {
                                f.params
                                    .iter()
                                    .enumerate()
                                    .filter(|(_, p)| p.capability == Capability::Modify)
                                    .map(|(i, _)| i)
                                    .collect()
                            })
                            .unwrap_or_default();
                        if modifies.is_empty() {
                            return self.call(name, &vals);
                        }
                        let (ret, finals) = self.call_capturing(name, &vals)?;
                        for i in modifies {
                            if let Expr::Var { name: vn, .. } = &args[i] {
                                for frame in scope.iter_mut().rev() {
                                    if let Some(slot) = frame.get_mut(vn) {
                                        slot.v = finals[i].clone();
                                        break;
                                    }
                                }
                            }
                        }
                        Ok(ret)
                    }
                }
            }
            Expr::Match { scrutinee, arms, .. } => {
                let sv = self.expr(scrutinee, scope)?;
                self.eval_match(sv, arms, scope)
            }
            Expr::Try { expr, .. } => {
                let v = self.expr(expr, scope)?;
                match v {
                    Val::Option(Some(inner)) => Ok(*inner),
                    Val::Option(None) => Err(Ctrl::Return(Val::Option(None))),
                    Val::Result(true, inner) => Ok(*inner),
                    Val::Result(false, e) => Err(Ctrl::Return(Val::Result(false, e))),
                    other => Err(format!("`?` on a non-Option/Result value {other:?}").into()),
                }
            }
            Expr::StructLit { name, fields, .. } => {
                let mut map = HashMap::new();
                for (fname, value) in fields {
                    let v = self.expr(value, scope)?;
                    map.insert(fname.clone(), v);
                }
                // Each field value flows through its declared field type —
                // sized-int wrapping and automatic validation for predicated
                // field types (`age: Age` from a raw Int64 runs Age's check).
                // Generic field types (Params) pass through coerce untouched.
                if let Some(Type::Record(rfields)) =
                    self.types.get(name.as_str()).map(|d| &d.base)
                {
                    for f in rfields {
                        if let Some(v) = map.remove(&f.name) {
                            map.insert(f.name.clone(), self.coerce(v, &f.ty)?);
                        }
                    }
                }
                // Enforce a cross-field `where` invariant, if the record declares
                // one (e.g. `{ start, end } where start < end`). The predicate
                // runs under the runtime evaluator with every field bound, so
                // Float/sized-int fields compare with exact runtime semantics.
                if let Some(decl) = self.types.get(name.as_str()) {
                    if let Some(pred) = &decl.predicate {
                        let mut env = vec![map
                            .iter()
                            .map(|(k, v)| (k.clone(), Slot::untyped(v.clone())))
                            .collect::<HashMap<_, _>>()];
                        match self.expr(pred, &mut env)? {
                            Val::Bool(true) => {}
                            Val::Bool(false) => {
                                return Err(format!(
                                    "validation failed: `{name}` violates its `where` clause"
                                )
                                .into())
                            }
                            other => {
                                return Err(format!(
                                    "cross-field predicate for `{name}` did not evaluate \
                                     to Bool (got {other:?})"
                                )
                                .into())
                            }
                        }
                    }
                }
                Ok(Val::Record(map))
            }
            Expr::Field { expr, field, .. } => {
                let v = self.expr(expr, scope)?;
                match v {
                    // `arr.length` is the element count (sugar for `alen`).
                    Val::Array(items) if field == "length" => Ok(Val::Int(items.len() as i64)),
                    // `str.length` is the byte length (matches `strlen`).
                    Val::Str(s) if field == "length" => Ok(Val::Int(s.len() as i64)),
                    Val::Record(map) => map
                        .get(field)
                        .cloned()
                        .ok_or_else(|| Ctrl::Err(format!("no field `{field}`"))),
                    other => Err(format!("field access on non-record {other:?}").into()),
                }
            }
            Expr::TryConstruct { name, args, .. } => {
                let v = self.expr(&args[0], scope)?;
                let decl = self
                    .types
                    .get(name.as_str())
                    .ok_or_else(|| Ctrl::Err(format!("unknown type `{name}`")))?;
                // Valid ⇒ Some(value); refinement fails ⇒ None (never aborts).
                if self.validates(decl, &v)? {
                    Ok(Val::Option(Some(Box::new(v))))
                } else {
                    Ok(Val::Option(None))
                }
            }
            Expr::ArrayLit { elems, .. } => {
                let mut vals = Vec::with_capacity(elems.len());
                for e in elems {
                    vals.push(self.expr(e, scope)?);
                }
                Ok(Val::Array(vals))
            }
            // A deterministic fork-join task: the callee is isolated (pure), so
            // running it eagerly here yields the same result any scheduler would.
            Expr::Spawn { name, args, .. } => {
                let mut vals = Vec::with_capacity(args.len());
                for a in args {
                    vals.push(self.expr(a, scope)?);
                }
                self.call(name, &vals)
            }
        }
    }

    /// Whether `v` satisfies `decl`'s refinement predicate (always true if none).
    ///
    /// The predicate is evaluated by the *runtime* evaluator with `value` bound
    /// — not by consteval — so every value kind the interpreter has (Float,
    /// sized ints, strings, `at()`, `=~`) validates with exactly its runtime
    /// semantics, and a predicate that traps (division by zero) traps the same
    /// way an ordinary expression does.
    fn validates(&self, decl: &TypeDecl, v: &Val) -> Result<bool, Ctrl> {
        let pred = match &decl.predicate {
            None => return Ok(true),
            Some(p) => p,
        };
        let mut scope =
            vec![HashMap::from([("value".to_string(), Slot::untyped(v.clone()))])];
        match self.expr(pred, &mut scope)? {
            Val::Bool(b) => Ok(b),
            other => Err(format!(
                "refinement for `{}` did not evaluate to Bool (got {other:?})",
                decl.name
            )
            .into()),
        }
    }

    /// Evaluate a `match` over an Option or Result, binding the payload.
    fn eval_match(
        &self,
        sv: Val,
        arms: &[MatchArm],
        scope: &mut Vec<HashMap<String, Slot>>,
    ) -> Result<Val, Ctrl> {
        for arm in arms {
            // (does this arm match?, payload bindings)
            let (matched, bindings): (bool, Vec<(String, Val)>) = match (&arm.pattern, &sv) {
                (Pattern::Some(b), Val::Option(Some(v))) => (true, vec![(b.clone(), (**v).clone())]),
                (Pattern::None, Val::Option(None)) => (true, vec![]),
                (Pattern::Ok(b), Val::Result(true, v)) => (true, vec![(b.clone(), (**v).clone())]),
                (Pattern::Err(b), Val::Result(false, v)) => (true, vec![(b.clone(), (**v).clone())]),
                (Pattern::Variant(n, binds), Val::Enum(vn, payload)) if n == vn => {
                    let bs = binds.iter().cloned().zip(payload.iter().cloned()).collect();
                    (true, bs)
                }
                _ => (false, vec![]),
            };
            if !matched {
                continue;
            }
            scope.push(HashMap::new());
            for (name, val) in bindings {
                scope.last_mut().unwrap().insert(name, Slot::untyped(val));
            }
            let result = self.expr(&arm.body, scope);
            scope.pop();
            return result;
        }
        Err("non-exhaustive match (should have been caught)".into())
    }

    fn binop(&self, op: BinOp, l: Val, r: Val) -> Result<Val, Ctrl> {
        use BinOp::*;
        // Float32 (possibly with a plain-Float literal sibling): round both to f32
        // and compute at single precision, matching native `float` instructions.
        if matches!(l, Val::Float32(_)) || matches!(r, Val::Float32(_)) {
            let to_f32 = |v: &Val| -> Result<f32, Ctrl> {
                match v {
                    Val::Float32(f) => Ok(*f),
                    Val::Float(f) => Ok(*f as f32),
                    _ => Err("type error in Float32 binop".into()),
                }
            };
            let (a, b) = (to_f32(&l)?, to_f32(&r)?);
            return Ok(match op {
                Add => Val::Float32(a + b),
                Sub => Val::Float32(a - b),
                Mul => Val::Float32(a * b),
                Div => Val::Float32(a / b),
                Lt => Val::Bool(a < b),
                LtEq => Val::Bool(a <= b),
                Gt => Val::Bool(a > b),
                GtEq => Val::Bool(a >= b),
                Eq => Val::Bool(a == b),
                NotEq => Val::Bool(a != b),
                Rem | And | Or | Match => {
                    return Err("type error in float binop (should have been caught)".into())
                }
            });
        }
        // Sized integers (possibly with a plain-Int literal sibling): compute in
        // i64, then wrap arithmetic back to the operand width (matching native iN).
        if matches!(l, Val::IntN { .. }) || matches!(r, Val::IntN { .. }) {
            let (bits, signed) = match (&l, &r) {
                (Val::IntN { bits, signed, .. }, _) | (_, Val::IntN { bits, signed, .. }) => {
                    (*bits, *signed)
                }
                _ => unreachable!(),
            };
            // Wrap BOTH operands to the sized type first: a plain-`Int` literal
            // sibling (`x < 300` on a UInt8) must be truncated exactly as the
            // native backend's iN registers truncate it — comparing or dividing
            // by the raw i64 would give a different answer.
            let x = match l {
                Val::IntN { v, .. } => wrap_intn(v, bits, signed),
                Val::Int(n) => wrap_intn(n, bits, signed),
                _ => return Err("type error in sized-int binop".into()),
            };
            let y = match r {
                Val::IntN { v, .. } => wrap_intn(v, bits, signed),
                Val::Int(n) => wrap_intn(n, bits, signed),
                _ => return Err("type error in sized-int binop".into()),
            };
            let mk = |v: i64| Val::IntN { v: wrap_intn(v, bits, signed), bits, signed };
            // The sized type's minimum, for the signed-overflow division trap
            // (MIN / -1 has no representable result; native sdiv traps on it).
            // Arithmetic shift sign-extends, so this is exact for bits = 8..64.
            let min_n: i64 = if signed { i64::MIN >> (64 - bits) } else { 0 };
            // Add/Sub/Mul are identical for signed/unsigned (two's complement);
            // Div/Rem and comparison differ — unsigned uses `u64` semantics.
            return Ok(match op {
                Add => mk(x.wrapping_add(y)),
                Sub => mk(x.wrapping_sub(y)),
                Mul => mk(x.wrapping_mul(y)),
                Div => {
                    if y == 0 {
                        return Err("division by zero".into());
                    }
                    if signed && x == min_n && y == -1 {
                        return Err("integer overflow in division".into());
                    }
                    mk(if signed {
                        x.wrapping_div(y)
                    } else {
                        (x as u64).wrapping_div(y as u64) as i64
                    })
                }
                Rem => {
                    if y == 0 {
                        return Err("remainder by zero".into());
                    }
                    if signed && x == min_n && y == -1 {
                        return Err("integer overflow in division".into());
                    }
                    mk(if signed {
                        x.wrapping_rem(y)
                    } else {
                        (x as u64).wrapping_rem(y as u64) as i64
                    })
                }
                Lt => Val::Bool(if signed { x < y } else { (x as u64) < (y as u64) }),
                LtEq => Val::Bool(if signed { x <= y } else { (x as u64) <= (y as u64) }),
                Gt => Val::Bool(if signed { x > y } else { (x as u64) > (y as u64) }),
                GtEq => Val::Bool(if signed { x >= y } else { (x as u64) >= (y as u64) }),
                Eq => Val::Bool(x == y),
                NotEq => Val::Bool(x != y),
                And | Or | Match => return Err("`&&`/`||` need Bool operands".into()),
            });
        }
        match (l, r) {
            (Val::Int(a), Val::Int(b)) => Ok(match op {
                // Wrapping two's complement — the language's defined overflow
                // semantics, matching native (and independent of the build
                // profile; bare `+` would panic in debug and wrap in release).
                Add => Val::Int(a.wrapping_add(b)),
                Sub => Val::Int(a.wrapping_sub(b)),
                Mul => Val::Int(a.wrapping_mul(b)),
                Div => {
                    if b == 0 {
                        return Err("division by zero".into());
                    }
                    if a == i64::MIN && b == -1 {
                        return Err("integer overflow in division".into());
                    }
                    Val::Int(a / b)
                }
                Rem => {
                    if b == 0 {
                        return Err("remainder by zero".into());
                    }
                    if a == i64::MIN && b == -1 {
                        return Err("integer overflow in division".into());
                    }
                    Val::Int(a % b)
                }
                Lt => Val::Bool(a < b),
                LtEq => Val::Bool(a <= b),
                Gt => Val::Bool(a > b),
                GtEq => Val::Bool(a >= b),
                Eq => Val::Bool(a == b),
                NotEq => Val::Bool(a != b),
                And | Or | Match => unreachable!("handled above"),
            }),
            (Val::Float(a), Val::Float(b)) => Ok(match op {
                Add => Val::Float(a + b),
                Sub => Val::Float(a - b),
                Mul => Val::Float(a * b),
                Div => Val::Float(a / b), // IEEE: /0.0 is inf/NaN, not a trap
                Lt => Val::Bool(a < b),
                LtEq => Val::Bool(a <= b),
                Gt => Val::Bool(a > b),
                GtEq => Val::Bool(a >= b),
                Eq => Val::Bool(a == b),
                NotEq => Val::Bool(a != b),
                Rem | And | Or | Match => {
                    return Err("type error in float binop (should have been caught)".into())
                }
            }),
            (Val::Bool(a), Val::Bool(b)) => match op {
                Eq => Ok(Val::Bool(a == b)),
                NotEq => Ok(Val::Bool(a != b)),
                _ => Err("type error in bool binop (should have been caught)".into()),
            },
            (Val::Str(a), Val::Str(b)) => match op {
                // `a + b` concatenates (replacing `concat`) — a fresh String.
                Add => Ok(Val::Str(format!("{a}{b}"))),
                Eq => Ok(Val::Bool(a == b)),
                NotEq => Ok(Val::Bool(a != b)),
                // `s =~ "pat"`: compile the (literal) pattern and full-match.
                Match => match crate::regex::compile(&b) {
                    Ok(dfa) => Ok(Val::Bool(dfa.matches(&a))),
                    Err(e) => Err(format!("invalid regex `{b}`: {e}").into()),
                },
                _ => Err("type error in string binop (should have been caught)".into()),
            },
            _ => Err("type error in binop (should have been caught)".into()),
        }
    }

    fn as_bool(&self, v: Val) -> Result<bool, Ctrl> {
        match v {
            Val::Bool(b) => Ok(b),
            other => Err(format!("expected Bool, found {other:?}").into()),
        }
    }

    /// The protocol-dispatch key for a runtime value (RFC-0002 §5): the scalar
    /// name for a scalar, or the enum's name for an enum value.
    fn val_type_key(&self, v: &Val) -> Option<String> {
        match v {
            Val::Int(_) => Some("Int64".to_string()),
            Val::Bool(_) => Some("Bool".to_string()),
            Val::Str(_) => Some("String".to_string()),
            Val::Enum(variant, _) => self.variant_enum.get(variant).cloned(),
            _ => None,
        }
    }

    /// Convert a value to `ty` at a typed boundary (let/param/return/field/
    /// element/assign). A plain integer flowing into a sized-integer slot wraps
    /// to that width, matching the native backend's `iN` truncation; a float in
    /// a `Float32` slot rounds to single precision.
    ///
    /// This is also where **automatic validation** happens: a value entering a
    /// predicated named type runs its `where` predicate and traps with the
    /// canonical `validation failed for \`T\`` when it does not hold. The walk
    /// is exhaustive — record fields, Option/Result payloads, and array
    /// elements are coerced (and therefore validated) recursively.
    fn coerce(&self, v: Val, ty: &Type) -> Result<Val, Ctrl> {
        match (ty, v) {
            (Type::IntN { bits, signed }, Val::Int(n)) => {
                Ok(Val::IntN { v: wrap_intn(n, *bits, *signed), bits: *bits, signed: *signed })
            }
            (Type::IntN { bits, signed }, Val::IntN { v, .. }) => {
                Ok(Val::IntN { v: wrap_intn(v, *bits, *signed), bits: *bits, signed: *signed })
            }
            // A float literal in a `Float32` slot rounds to single precision; an
            // already-f32 value stays put.
            (Type::Float32, Val::Float(f)) => Ok(Val::Float32(f as f32)),
            (Type::Named(n), v) => {
                let Some(decl) = self.types.get(n.as_str()) else { return Ok(v) };
                // Coerce toward the base first (a record base coerces fields;
                // a scalar base wraps), then run the predicate on the result.
                let v = self.coerce(v, &decl.base)?;
                if let Some(pred) = &decl.predicate {
                    // A record base has a cross-field predicate (field names in
                    // scope); a scalar base binds `value`.
                    let holds = if matches!(decl.base, Type::Record(_)) {
                        match &v {
                            Val::Record(map) => {
                                let mut env = vec![map
                            .iter()
                            .map(|(k, v)| (k.clone(), Slot::untyped(v.clone())))
                            .collect::<HashMap<_, _>>()];
                                match self.expr(pred, &mut env)? {
                                    Val::Bool(b) => b,
                                    other => {
                                        return Err(format!(
                                            "cross-field predicate for `{n}` did not \
                                             evaluate to Bool (got {other:?})"
                                        )
                                        .into())
                                    }
                                }
                            }
                            _ => true, // not a record value — nothing to check
                        }
                    } else {
                        self.validates(decl, &v)?
                    };
                    if !holds {
                        let msg = if matches!(decl.base, Type::Record(_)) {
                            format!("validation failed: `{n}` violates its `where` clause")
                        } else {
                            format!("validation failed for `{n}`")
                        };
                        return Err(msg.into());
                    }
                }
                Ok(v)
            }
            (Type::Record(fields), Val::Record(mut map)) => {
                for f in fields {
                    if let Some(fv) = map.remove(&f.name) {
                        map.insert(f.name.clone(), self.coerce(fv, &f.ty)?);
                    }
                }
                Ok(Val::Record(map))
            }
            (Type::Option(inner), Val::Option(Some(p))) => {
                Ok(Val::Option(Some(Box::new(self.coerce(*p, inner)?))))
            }
            (Type::Result(tok, terr), Val::Result(is_ok, p)) => {
                let inner = if is_ok { tok } else { terr };
                Ok(Val::Result(is_ok, Box::new(self.coerce(*p, inner)?)))
            }
            (Type::Array(inner), Val::Array(items)) | (Type::ArrayN(inner, _), Val::Array(items)) => {
                let mut out = Vec::with_capacity(items.len());
                for it in items {
                    out.push(self.coerce(it, inner)?);
                }
                Ok(Val::Array(out))
            }
            (_, v) => Ok(v),
        }
    }

    fn as_ref(&self, v: &Val) -> Result<(usize, u64), Ctrl> {
        match v {
            Val::Ref { slot, gen } => Ok((*slot, *gen)),
            other => Err(format!("expected Ref, found {other:?}").into()),
        }
    }

    // ---- generational-reference cell slab (RFC-0004 §4, Path B) ----------

    /// The trap raised when a released reference is used — the whole point of
    /// generational references (native prints a message and exits 1 to match).
    fn stale() -> Ctrl {
        Ctrl::Err("reference used after release".into())
    }

    fn cell_alloc(&self, v: Val) -> Result<Val, Ctrl> {
        let mut cells = self.cells.borrow_mut();
        if let Some(slot) = self.free.borrow_mut().pop() {
            cells[slot].val = v; // generation already bumped at release
            return Ok(Val::Ref { slot, gen: cells[slot].gen });
        }
        let slot = cells.len();
        // The native slab is a fixed 65536-slot array; mirror its capacity (and
        // its trap message) exactly rather than growing without bound.
        if slot >= 65536 {
            return Err("out of reference cells".into());
        }
        cells.push(CellSlot { gen: 0, val: v });
        Ok(Val::Ref { slot, gen: 0 })
    }

    fn cell_get(&self, slot: usize, gen: u64) -> Result<Val, Ctrl> {
        let cells = self.cells.borrow();
        match cells.get(slot) {
            Some(c) if c.gen == gen => Ok(c.val.clone()),
            _ => Err(Self::stale()),
        }
    }

    fn cell_set(&self, slot: usize, gen: u64, v: Val) -> Result<(), Ctrl> {
        let mut cells = self.cells.borrow_mut();
        match cells.get_mut(slot) {
            Some(c) if c.gen == gen => {
                c.val = v;
                Ok(())
            }
            _ => Err(Self::stale()),
        }
    }

    fn cell_release(&self, slot: usize, gen: u64) -> Result<(), Ctrl> {
        let mut cells = self.cells.borrow_mut();
        match cells.get_mut(slot) {
            Some(c) if c.gen == gen => {
                c.gen += 1; // stale refs (old gen) now fail the check
                drop(cells);
                self.free.borrow_mut().push(slot);
                Ok(())
            }
            _ => Err(Self::stale()),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::run;

    #[test]
    fn arithmetic_and_return() {
        assert_eq!(run("fn main() -> Int64 { return 2 + 3 * 4; }").unwrap(), 14);
    }

    #[test]
    fn functions_and_recursion() {
        let src = "
            fn fib(n: Int64) -> Int64 {
                if n < 2 { return n; }
                return fib(n - 1) + fib(n - 2);
            }
            fn main() -> Int64 { return fib(10); }
        ";
        assert_eq!(run(src).unwrap(), 55);
    }

    #[test]
    fn option_and_match() {
        let src = "
            fn sd(a: Int64, b: Int64) -> Option<Int64> {
                if b == 0 { return None; }
                return Some(a / b);
            }
            fn uw(o: Option<Int64>, f: Int64) -> Int64 {
                return match o { Some(x) => x, None => f };
            }
            fn main() -> Int64 { return uw(sd(10, 2), 0) + uw(sd(1, 0), 100); }
        ";
        assert_eq!(run(src).unwrap(), 105); // 5 + 100
    }

    #[test]
    fn result_and_question_mark() {
        // `?` propagates Err out of `chain`, so chain(0) returns Err(-1) and the
        // final match yields the fallback.
        let src = "
            fn checked(n: Int64) -> Result<Int64, Int64> {
                if n == 0 { return Err(0 - 1); }
                return Ok(n);
            }
            fn chain(n: Int64) -> Result<Int64, Int64> {
                let x = checked(n)?;      // early-returns Err when n == 0
                return Ok(x + 1);
            }
            fn main() -> Int64 {
                let a = match chain(5) { Ok(v) => v, Err(e) => e };   // 6
                let b = match chain(0) { Ok(v) => v, Err(e) => e };   // -1
                return a + b;             // 5
            }
        ";
        assert_eq!(run(src).unwrap(), 5);
    }

    #[test]
    fn generational_reference_roundtrip() {
        let src = "fn main() -> Int64 { \
                       let c = cell(10); set(c, get(c) + 5); \
                       let v = get(c); release(c); return v; }";
        assert_eq!(run(src).unwrap(), 15);
    }

    #[test]
    fn linked_list_via_option_ref() {
        // A nil-terminated recursive list: Option<Ref<Node>> holds each edge.
        let src = "
            type Node = { value: Int64, next: Option<Ref<Node>> };
            fn sum(o: Option<Ref<Node>>) -> Int64 {
                return match o { Some(r) => get(r).value + sum(get(r).next), None => 0 };
            }
            fn main() -> Int64 {
                let n2 = cell(Node { value: 2, next: None });
                let n1 = cell(Node { value: 1, next: Some(n2) });
                return sum(Some(n1));
            }
        ";
        assert_eq!(run(src).unwrap(), 3);
    }

    #[test]
    fn str_and_parse_roundtrip() {
        let src = "fn main() -> Int64 { \
                       let s = (0 - 123).toString(); \
                       return match parse(s) { Some(n) => n, None => 0 }; }";
        assert_eq!(run(src).unwrap(), -123);
    }

    #[test]
    fn parse_rejects_non_integers() {
        let cases = [("\"12x\"", -1), ("\"\"", -1), ("\"-\"", -1), ("\" 5\"", -1), ("\"42\"", 42)];
        for (lit, want) in cases {
            let src = format!(
                "fn main() -> Int64 {{ return match parse({lit}) {{ Some(n) => n, None => 0 - 1 }}; }}"
            );
            assert_eq!(run(&src).unwrap(), want, "parse({lit})");
        }
    }

    #[test]
    fn result_holds_non_int_payloads() {
        // Ok carries a Ref, Err carries a String.
        let src = "
            fn lookup(k: Int64) -> Result<Ref<Int64>, String> {
                if k == 0 { return Err(\"nope\"); }
                return Ok(cell(k * 10));
            }
            fn main() -> Int64 {
                let a = match lookup(5) { Ok(r) => get(r), Err(e) => 0 - e.length };
                let b = match lookup(0) { Ok(r) => get(r), Err(e) => 0 - e.length };
                return a + b;  // 50 + (-4)
            }
        ";
        assert_eq!(run(src).unwrap(), 46);
    }

    #[test]
    fn fixed_array_literal_and_index() {
        let src = "fn main() -> Int64 { let a: Array<Int64, 4> = [10, 20, 30, 40]; \
                   let mut s = 0; let mut i = 0; \
                   while i < alen(a) { s = s + at(a, i); i = i + 1; } return s; }";
        assert_eq!(run(src).unwrap(), 100);
    }

    #[test]
    fn fixed_array_out_of_bounds_errors() {
        let src = "fn main() -> Int64 { let a: Array<Int64, 2> = [1, 2]; return at(a, 4); }";
        assert!(run(src).unwrap_err().contains("out of bounds"));
    }

    #[test]
    fn growable_array_push_and_read() {
        let src = "fn main() -> Int64 { \
                       let mut a: Array<Int64> = array(); \
                       let mut i = 0; \
                       while i < 6 { a = push(a, i * i); i = i + 1; } \
                       let mut s = 0; let mut j = 0; \
                       while j < alen(a) { s = s + at(a, j); j = j + 1; } \
                       return s; }"; // 0+1+4+9+16+25 = 55
        assert_eq!(run(src).unwrap(), 55);
    }

    #[test]
    fn array_index_out_of_bounds_errors() {
        let src = "fn main() -> Int64 { let mut a: Array<Int64> = array(); \
                   a = push(a, 1); return at(a, 3); }";
        assert!(run(src).unwrap_err().contains("out of bounds"));
    }

    #[test]
    fn for_over_fixed_array() {
        let src = "fn main() -> Int64 { let a: Array<Int64, 5> = [0, 1, 4, 9, 16]; \
                   let mut s = 0; for x in a { s = s + x; } return s; }";
        assert_eq!(run(src).unwrap(), 30);
    }

    #[test]
    fn for_over_growable_array() {
        let src = "fn main() -> Int64 { let mut a: Array<Int64> = array(); \
                   let mut i = 0; while i < 6 { a = push(a, i * i); i = i + 1; } \
                   let mut s = 0; for x in a { s = s + x; } return s; }"; // 0+1+4+9+16+25
        assert_eq!(run(src).unwrap(), 55);
    }

    #[test]
    fn for_over_empty_array_runs_zero_times() {
        let src = "fn main() -> Int64 { let a: Array<Int64> = array(); \
                   let mut s = 7; for x in a { s = s + x; } return s; }";
        assert_eq!(run(src).unwrap(), 7);
    }

    #[test]
    fn for_loop_variable_is_scoped_to_body() {
        // `x` must not leak past the loop — referencing it after is unbound.
        let src = "fn main() -> Int64 { let a: Array<Int64, 2> = [1, 2]; \
                   for x in a { let y = x; } return x; }";
        assert!(run(src).is_err());
    }

    #[test]
    fn for_body_early_return() {
        // Returning from inside the loop stops iteration immediately.
        let src = "fn firstOver(a: Array<Int64, 4>, t: Int64) -> Int64 { \
                   for x in a { if x > t { return x; } } return 0 - 1; } \
                   fn main() -> Int64 { let a: Array<Int64, 4> = [3, 8, 1, 9]; \
                   return firstOver(a, 5); }"; // first element > 5 is 8
        assert_eq!(run(src).unwrap(), 8);
    }

    #[test]
    fn for_over_non_array_is_rejected() {
        let src = "fn main() -> Int64 { let n = 3; for x in n { } return 0; }";
        assert!(run(src).unwrap_err().contains("Array"));
    }

    #[test]
    fn method_index_and_length_surface() {
        // `[]`, `.push`, `.length`, and `[i]` desugar to array()/push/alen/at.
        let src = "fn main() -> Int64 { let mut a: Array<Int64> = []; \
                   a.push(10); a.push(20); a.push(30); \
                   return a.length + a[0] + a[2]; }"; // 3 + 10 + 30
        assert_eq!(run(src).unwrap(), 43);
    }

    #[test]
    fn method_push_writes_back() {
        // `a.push(x);` as a statement mutates `a` in place (write-back).
        let src = "fn main() -> Int64 { let mut a: Array<Int64> = []; \
                   let mut i = 0; while i < 5 { a.push(i); i = i + 1; } \
                   let mut s = 0; for x in a { s = s + x; } return s; }"; // 0+1+2+3+4
        assert_eq!(run(src).unwrap(), 10);
    }

    #[test]
    fn drop_then_use_is_a_compile_error() {
        // `drop` consumes: using the value afterward must be rejected.
        let src = "fn main() -> Int64 { let mut a: Array<Int64> = []; a.push(1); \
                   drop a; return a.length; }";
        assert!(run(src).is_err());
    }

    #[test]
    fn drop_of_reference_releases_it() {
        // After `drop r`, the reference is released, so reading it would trap —
        // but here we just confirm a well-formed drop runs and returns.
        let src = "fn main() -> Int64 { let r = cell(7); let v = get(r); \
                   drop r; return v; }";
        assert_eq!(run(src).unwrap(), 7);
    }

    #[test]
    fn drop_of_non_heap_is_rejected() {
        let src = "fn main() -> Int64 { let n = 5; drop n; return 0; }";
        assert!(run(src).unwrap_err().contains("heap"));
    }

    #[test]
    fn string_interpolation_renders_scalars() {
        // `\{ }` holes render Int/Bool/String; literal braces are untouched. The
        // program returns the interpolated string's length so we can assert it.
        let src = "fn main() -> Int64 { let n = 42; let ok = true; \
                   let s = \"n=\\{n} ok=\\{ok} {lit}\"; return s.length; }";
        // "n=42 ok=true {lit}" -> 18 characters
        assert_eq!(run(src).unwrap(), 18);
    }

    #[test]
    fn interpolation_evaluates_hole_expressions() {
        let src = "fn main() -> Int64 { let a = 3; let b = 4; \
                   let s = \"\\{a * b}\"; return s.length; }"; // "12" -> len 2
        assert_eq!(run(src).unwrap(), 2);
    }

    #[test]
    fn str_renders_bool_and_string() {
        let src = "fn main() -> Int64 { let s = false.toString(); return s.length; }"; // "false" -> 5
        assert_eq!(run(src).unwrap(), 5);
    }

    #[test]
    fn str_renders_sized_int() {
        // A signed Int32 renders by value; an unsigned UInt8 renders its magnitude.
        let s = "fn main() -> Int64 { let a: Int32 = 42; let b: UInt8 = 200; \
                 let s = \"\\{a}/\\{b + b}\"; return s.length; }"; // "42/144" -> 6
        assert_eq!(run(s).unwrap(), 6);
    }

    #[test]
    fn str_renders_uint64_above_i64_max() {
        // The full 64-bit magnitude renders (not a signed reinterpretation).
        let s = "fn main() -> Int64 { let n: UInt64 = 10000000000000000000; \
                 let s = n.toString(); return s.length; }"; // 20 digits
        assert_eq!(run(s).unwrap(), 20);
    }

    #[test]
    fn str_renders_float_to_six_decimals() {
        let s = "fn main() -> Int64 { let s = (3.14159).toString(); return s.length; }"; // "3.141590" -> 8
        assert_eq!(run(s).unwrap(), 8);
    }

    #[test]
    fn float_arithmetic_and_comparison() {
        // 1.5 * 2.5 = 3.75 > 3.0 → 1
        let src = "fn main() -> Int64 { let a = 1.5; let b = 2.5; \
                   if a * b > 3.0 { return 1; } return 0; }";
        assert_eq!(run(src).unwrap(), 1);
    }

    #[test]
    fn float_through_function_and_negation() {
        let src = "fn half(x: Float64) -> Float64 { return x / 2.0; } \
                   fn main() -> Int64 { let h = half(5.0); \
                   if h == 2.5 { if -h < 0.0 { return 7; } } return 0; }";
        assert_eq!(run(src).unwrap(), 7);
    }

    #[test]
    fn float_to_int_truncates_toward_zero() {
        let src = "fn main() -> Int64 { let f = 3.9; return Int64(f); }";
        assert_eq!(run(src).unwrap(), 3);
    }

    #[test]
    fn int_to_float_and_back() {
        let src = "fn main() -> Int64 { let f = Float64(7); let g = f + 0.5; return Int64(g); }"; // 7.5 -> 7
        assert_eq!(run(src).unwrap(), 7);
    }

    #[test]
    fn float32_rounds_to_single_precision() {
        // 2^24 + 1 is exact in f64 but rounds to 2^24 in f32, so `Int(..)` differs.
        let f32 = "fn main() -> Int64 { let x: Float32 = 16777217.0; return Int64(x); }";
        assert_eq!(run(f32).unwrap(), 16777216);
        let f64 = "fn main() -> Int64 { let x: Float64 = 16777217.0; return Int64(x); }";
        assert_eq!(run(f64).unwrap(), 16777217);
    }

    #[test]
    fn float32_arithmetic_stays_single_precision() {
        // Adding 1.0 to 1e8 is below the f32 ULP → lost; f64 keeps it.
        let src = "fn addf(a: Float32, b: Float32) -> Float32 { return a + b; } \
                   fn main() -> Int64 { let g: Float32 = 100000000.0; return Int64(addf(g, 1.0)); }";
        assert_eq!(run(src).unwrap(), 100000000);
    }

    #[test]
    fn float32_widens_to_float64_exactly() {
        // 0.5 is exact in both; Float32 -> Float64 -> Int round-trips its value.
        let src = "fn main() -> Int64 { let x: Float32 = 2.5; let d = Float64(x); \
                   if d == 2.5 { return 1; } return 0; }";
        assert_eq!(run(src).unwrap(), 1);
    }

    #[test]
    fn float32_literal_adapts_to_sibling() {
        // A plain float literal takes the Float32 sibling's precision.
        let src = "fn main() -> Int64 { let h: Float32 = 1.5; let r = h + 2.5; return Int64(r); }";
        assert_eq!(run(src).unwrap(), 4);
    }

    #[test]
    fn int_to_int32_wraps_and_back() {
        // 5_000_000_000 wraps into i32 to 705032704; Int(..) sext's it back.
        let src = "fn main() -> Int64 { let big = 5000000000; return Int64(Int32(big)); }";
        assert_eq!(run(src).unwrap(), 705032704);
    }

    #[test]
    fn int8_conversion_wraps() {
        let src = "fn main() -> Int64 { return Int64(Int8(300)); }"; // 300 & 0xFF as i8 = 44
        assert_eq!(run(src).unwrap(), 44);
    }

    #[test]
    fn rejects_conversion_of_non_number() {
        let src = "fn main() -> Int64 { let x = Int64(\"hi\"); return 0; }";
        assert!(run(src).unwrap_err().contains("converts a number"));
    }

    #[test]
    fn int64_is_an_alias_for_int() {
        let src = "fn f(n: Int64) -> Int64 { return n + 1; } \
                   fn main() -> Int64 { let x: Int64 = 41; return f(x); }";
        assert_eq!(run(src).unwrap(), 42);
    }

    #[test]
    fn rejects_int_float_mixing() {
        let src = "fn main() -> Int64 { let a = 1 + 2.0; return 0; }";
        assert!(run(src).unwrap_err().contains("matching numeric"));
    }

    #[test]
    fn rejects_float_assigned_to_int() {
        let src = "fn main() -> Int64 { let x: Int64 = 1.5; return x; }";
        assert!(run(src).is_err());
    }

    #[test]
    fn int32_overflow_wraps() {
        // 2e9 + 2e9 = 4e9 wraps at 32 bits to -294967296.
        let src = "fn main() -> Int64 { let a: Int32 = 2000000000; let b: Int32 = 2000000000; \
                   let c = a + b; if c < 0 { return 1; } return 0; }";
        assert_eq!(run(src).unwrap(), 1);
    }

    #[test]
    fn int8_wraps_at_eight_bits() {
        // 100 + 100 = 200 wraps at 8 bits (signed) to -56.
        let src = "fn wrap(a: Int8, b: Int8) -> Int8 { return a + b; } \
                   fn main() -> Int64 { let x: Int8 = 100; let r = wrap(x, x); \
                   if r < 0 { return 1; } return 0; }";
        assert_eq!(run(src).unwrap(), 1);
    }

    #[test]
    fn uint8_wraps_into_magnitude_range() {
        // 200 + 200 = 400 wraps at 8 bits (unsigned) to 144 — stays non-negative.
        let src = "fn main() -> Int64 { let x: UInt8 = 200; let r = x + x; return Int64(r); }";
        assert_eq!(run(src).unwrap(), 144);
    }

    #[test]
    fn uint8_subtraction_wraps_below_zero() {
        // 200 - 250 = -50 wraps to 206 in unsigned 8-bit space.
        let src = "fn main() -> Int64 { let x: UInt8 = 200; let r = x - 250; return Int64(r); }";
        assert_eq!(run(src).unwrap(), 206);
    }

    #[test]
    fn uint_uses_unsigned_division() {
        // A UInt64 above i64::MAX divides unsigned (signed sdiv would give a
        // different, negative-influenced quotient).
        let src = "fn main() -> Int64 { let n: UInt64 = 10000000000000000000; \
                   let q = n / 3; if q == 3333333333333333333 { return 1; } return 0; }";
        assert_eq!(run(src).unwrap(), 1);
    }

    #[test]
    fn uint_comparison_is_unsigned() {
        // As unsigned, 10e18 (>i64::MAX, stored as a negative i64) is GREATER
        // than 5 — a signed comparison would wrongly rank it below.
        let src = "fn main() -> Int64 { let big: UInt64 = 10000000000000000000; \
                   let small: UInt64 = 5; if big > small { return 1; } return 0; }";
        assert_eq!(run(src).unwrap(), 1);
    }

    #[test]
    fn uint32_holds_value_above_int32_max() {
        // 4_000_000_000 overflows Int32 but fits UInt32.
        let src = "fn main() -> Int64 { return Int64(UInt32(Int64(4000000000))); }";
        assert_eq!(run(src).unwrap(), 4000000000);
    }

    #[test]
    fn sized_int_no_overflow_is_normal() {
        let src = "fn main() -> Int64 { let a: Int32 = 5; let b = a * 3; \
                   if b == 15 { return 1; } return 0; }";
        assert_eq!(run(src).unwrap(), 1);
    }

    #[test]
    fn rejects_mixing_different_int_widths() {
        let src = "fn main() -> Int64 { let a: Int32 = 1; let b: Int8 = 2; let c = a + b; return 0; }";
        assert!(run(src).unwrap_err().contains("matching numeric"));
    }

    #[test]
    fn tagged_template_passes_parts_and_boxed_values() {
        // A `sql` tag receives literal parts + boxed values; the structure comes
        // only from parts (here we return $N per hole and check the length).
        let src = "fn sql(parts: Array<String>, values: Array<Value>) -> Int64 { \
                       return parts.length + values.length; } \
                   fn main() -> Int64 { let a = 1; let b = 2; \
                       return sql\"x\\{a}y\\{b}z\"; }"; // parts=3, values=2 -> 5
        assert_eq!(run(src).unwrap(), 5);
    }

    #[test]
    fn tagged_template_values_are_matchable_and_typed() {
        // The boxed values decode back to their original scalars via `match`.
        let src = "fn sql(parts: Array<String>, values: Array<Value>) -> Int64 { \
                       return match values[0] { IntVal(n) => n, BoolVal(b) => 0, StrVal(s) => s.length }; } \
                   fn main() -> Int64 { let x = 41; return sql\"n=\\{x}\"; }";
        assert_eq!(run(src).unwrap(), 41);
    }

    #[test]
    fn schema_of_extracts_where_bounds() {
        // `schemaOf(Port)` reads the `where` predicate at compile time.
        let src = "type Port = Int64 where value >= 1 && value <= 65535; \
                   fn optOr(o: Option<Int64>, d: Int64) -> Int64 { \
                       return match o { Some(n) => n, None => d }; } \
                   fn main() -> Int64 { let s = schemaOf(Port); \
                       return optOr(s.min, 0) + optOr(s.max, 0); }"; // 1 + 65535
        assert_eq!(run(src).unwrap(), 65536);
    }

    /// The enriched `Schema`: name, base spelling (incl. sized ints), `///`
    /// doc, `multipleOf`, string length bounds, and the regex pattern.
    #[test]
    fn schema_of_enriched_fields() {
        let src = "/// A lowercase handle.\n\
                   type Username = String where value.length >= 3 && value.length <= 16 && value =~ \"[a-z]+\"\n\
                   type Even = Int64 where value % 2 == 0\n\
                   type Byte = UInt8\n\
                   fn optOr(o: Option<Int64>, d: Int64) -> Int64 {\n\
                       return match o { Some(n) => n, None => d }\n\
                   }\n\
                   fn main() -> Int64 {\n\
                       let u = schemaOf(Username)\n\
                       let e = schemaOf(Even)\n\
                       let b = schemaOf(Byte)\n\
                       let mut n = 0\n\
                       if u.name == \"Username\" { n = n + 1 }\n\
                       if u.base == \"String\" { n = n + 1 }\n\
                       if optOr(u.minLength, 0) == 3 { n = n + 1 }\n\
                       if optOr(u.maxLength, 0) == 16 { n = n + 1 }\n\
                       if match u.pattern { Some(p) => p == \"[a-z]+\", None => false } { n = n + 1 }\n\
                       if match u.doc { Some(d) => true, None => false } { n = n + 1 }\n\
                       if optOr(e.multipleOf, 0) == 2 { n = n + 1 }\n\
                       if b.base == \"UInt8\" { n = n + 1 }\n\
                       if match b.doc { Some(d) => false, None => true } { n = n + 1 }\n\
                       return n\n\
                   }";
        assert_eq!(run(src).unwrap(), 9);
    }

    #[test]
    fn schema_of_unbounded_type_has_no_bounds() {
        let src = "type Id = Int64; \
                   fn none(o: Option<Int64>) -> Int64 { return match o { Some(n) => 1, None => 0 }; } \
                   fn main() -> Int64 { let s = schemaOf(Id); return none(s.min) + none(s.max); }";
        assert_eq!(run(src).unwrap(), 0); // both None
    }

    #[test]
    fn schema_of_rejects_a_non_type() {
        let src = "fn main() -> Int64 { let x = 5; let s = schemaOf(x); return 0; }";
        assert!(run(src).unwrap_err().contains("not a type"));
    }

    #[test]
    fn string_length_field() {
        let src = "fn main() -> Int64 { let s = \"hello\"; return s.length; }";
        assert_eq!(run(src).unwrap(), 5);
    }

    #[test]
    fn string_indexing_and_char_literal() {
        // `s[1]` is the byte 'e' (101); a char literal is that byte value.
        let src = "fn main() -> Int64 { let s = \"hello\"; return s[1]; }";
        assert_eq!(run(src).unwrap(), 101);
        let cmp = "fn main() -> Int64 { let s = \"hello\"; if s[0] == 'h' { return 1; } return 0; }";
        assert_eq!(run(cmp).unwrap(), 1);
    }

    #[test]
    fn string_index_out_of_bounds_traps() {
        let src = "fn main() -> Int64 { let s = \"hi\"; return s[5]; }";
        assert!(run(src).unwrap_err().contains("out of bounds"));
    }

    #[test]
    fn unicode_bytes_vs_code_points() {
        // "café": 5 UTF-8 bytes but 4 code points; `é` is U+00E9 = 233.
        let bytes = "fn main() -> Int64 { return bytes(\"caf\\u{e9}\").length; }";
        assert_eq!(run(bytes).unwrap(), 5);
        let chars = "fn main() -> Int64 { return chars(\"caf\\u{e9}\").length; }";
        assert_eq!(run(chars).unwrap(), 4);
        let cp = "fn main() -> Int64 { return chars(\"caf\\u{e9}\")[3]; }";
        assert_eq!(run(cp).unwrap(), 233);
    }

    #[test]
    fn code_point_iteration_and_emoji() {
        // A 4-byte emoji is a single code point.
        let len = "fn main() -> Int64 { return \"\\u{1F600}\".length; }"; // 4 bytes
        assert_eq!(run(len).unwrap(), 4);
        let one = "fn main() -> Int64 { return chars(\"\\u{1F600}\").length; }"; // 1 char
        assert_eq!(run(one).unwrap(), 1);
        let val = "fn main() -> Int64 { return chars(\"\\u{1F600}\")[0]; }";
        assert_eq!(run(val).unwrap(), 128512);
    }

    #[test]
    fn unicode_char_literal() {
        // A non-ASCII char literal is its Unicode scalar value.
        let src = "fn main() -> Int64 { return '\\u{e9}'; }";
        assert_eq!(run(src).unwrap(), 233);
    }

    #[test]
    fn encoding_helpers_roundtrip() {
        use super::{base64_decode, base64_encode, hex_decode, hex_encode, url_decode, url_encode};
        assert_eq!(hex_encode("Hi"), "4869");
        assert_eq!(hex_decode("4869").as_deref(), Some("Hi"));
        assert_eq!(base64_encode("Hello"), "SGVsbG8=");
        assert_eq!(base64_decode("SGVsbG8=").as_deref(), Some("Hello"));
        assert_eq!(url_encode("a b&c"), "a%20b%26c");
        assert_eq!(url_decode("a%20b%26c").as_deref(), Some("a b&c"));
        // A UTF-8 round-trip through base64.
        assert_eq!(base64_decode(&base64_encode("café")).as_deref(), Some("café"));
    }

    #[test]
    fn encoding_rejects_bad_input() {
        use super::{base64_decode, hex_decode, url_decode};
        assert_eq!(hex_decode("zz"), None); // non-hex
        assert_eq!(hex_decode("abc"), None); // odd length
        assert_eq!(hex_decode("ff"), None); // 0xFF is not valid UTF-8
        assert_eq!(base64_decode("bad"), None); // length not a multiple of 4
        assert_eq!(base64_decode("////"), None); // decodes to non-UTF-8 bytes
        assert_eq!(url_decode("%ZZ"), None); // bad percent escape
    }

    #[test]
    fn encoding_builtins_in_program() {
        // Exercised end-to-end (checker + interp) with an Option result.
        let src = "fn main() -> Int64 { \
                   let d = base64Decode(base64Encode(\"hey\")); \
                   return match d { Some(s) => s.length, None => 0 }; }";
        assert_eq!(run(src).unwrap(), 3);
    }

    #[test]
    fn string_iteration_sums_bytes() {
        // 'a'(97) + 'b'(98) + 'c'(99) = 294.
        let src = "fn main() -> Int64 { let s = \"abc\"; let mut t = 0; \
                   for c in s { t = t + c; } return t; }";
        assert_eq!(run(src).unwrap(), 294);
    }

    #[test]
    fn string_predicate_methods() {
        let c = "fn main() -> Int64 { if contains(\"hello\", \"ell\") { return 1; } return 0; }";
        assert_eq!(run(c).unwrap(), 1);
        let s = "fn main() -> Int64 { if startsWith(\"hello\", \"he\") { return 1; } return 0; }";
        assert_eq!(run(s).unwrap(), 1);
        let e = "fn main() -> Int64 { if endsWith(\"hello\", \"lo\") { return 1; } return 0; }";
        assert_eq!(run(e).unwrap(), 1);
        // `endsWith` guards against a suffix longer than the string.
        let g = "fn main() -> Int64 { if endsWith(\"hi\", \"ahoy\") { return 1; } return 0; }";
        assert_eq!(run(g).unwrap(), 0);
    }

    #[test]
    fn indexing_in_refinement_predicate() {
        let ok = "type G = String where value.length >= 1 && value[0] == 'H'; \
                  fn mk(s: String) -> G { return G(s); } \
                  fn main() -> Int64 { let g = mk(\"Hi\"); return g.length; }";
        assert_eq!(run(ok).unwrap(), 2);
        // A provably-wrong constant is rejected at compile time (via consteval).
        let bad = "type G = String where value.length >= 1 && value[0] == 'H'; \
                   fn main() -> Int64 { let g = G(\"bye\"); return 0; }";
        assert!(run(bad).unwrap_err().contains("does not satisfy `G`"));
    }

    #[test]
    fn validated_string_accepts_valid_value() {
        let src = "type Name = String where value.length >= 3; \
                   fn mk(s: String) -> Name { return Name(s); } \
                   fn main() -> Int64 { let n = mk(\"bob\"); return n.length; }";
        assert_eq!(run(src).unwrap(), 3);
    }

    #[test]
    fn validated_string_traps_on_too_short() {
        // Runtime construction of an invalid string aborts (matches native exit 1).
        let src = "type Name = String where value.length >= 3; \
                   fn mk(s: String) -> Name { return Name(s); } \
                   fn main() -> Int64 { let n = mk(\"x\"); return 0; }";
        assert!(run(src).unwrap_err().contains("validation failed for `Name`"));
    }

    #[test]
    fn cross_field_record_valid_and_invalid() {
        let ok = "type R = { a: Int64, b: Int64 } where a < b; \
                  fn mk(x: Int64, y: Int64) -> R { return R { a: x, b: y }; } \
                  fn main() -> Int64 { let r = mk(1, 2); return r.b; }";
        assert_eq!(run(ok).unwrap(), 2);
        let bad = "type R = { a: Int64, b: Int64 } where a < b; \
                   fn mk(x: Int64, y: Int64) -> R { return R { a: x, b: y }; } \
                   fn main() -> Int64 { let r = mk(5, 1); return 0; }";
        assert!(run(bad).unwrap_err().contains("violates its `where`"));
    }

    #[test]
    fn auto_release_recycles_the_slab() {
        // A non-escaping cell per iteration: the inferred release returns each
        // slot to the slab, so 70k allocations fit in 65536 cells — the
        // interpreter executes the same drop plan as the native backend.
        let src = "fn main() -> Int64 { \
                       let mut i = 0 \
                       let mut last = 0 \
                       while i < 70000 { \
                           let c = cell(i) \
                           set(c, get(c) + 1) \
                           last = get(c) \
                           i = i + 1 \
                       } \
                       if last == 70000 { return 1 } return 0 }";
        assert_eq!(run(src).unwrap(), 1);
    }

    #[test]
    fn slab_exhaustion_traps_like_native() {
        // Cells that ESCAPE (aliased) are not auto-released; the 65537th live
        // allocation must trap with the native slab's exact message.
        let src = "fn main() -> Int64 { \
                       let mut i = 0 \
                       while i < 70000 { \
                           let c = cell(1) \
                           let d = c \
                           i = i + 1 \
                       } \
                       return 0 }";
        let e = run(src).unwrap_err();
        assert_eq!(e, "out of reference cells");
    }

    #[test]
    fn validation_trap_message_is_canonical() {
        let src = "type Age = Int64 where value >= 18; \
                   fn mk(n: Int64) -> Age { return Age(n); } \
                   fn main() -> Int64 { let a = mk(5); return 0; }";
        assert_eq!(run(src).unwrap_err(), "validation failed for `Age`");
    }

    #[test]
    fn auto_validation_traps_dynamic_violations_at_each_boundary() {
        // Argument boundary.
        let arg = "type Age = Int64 where value >= 18 \
                   fn g(a: Age) -> Int64 { return a } \
                   fn main() -> Int64 { let mut x = 30 x = x - 25 return g(x) }";
        assert_eq!(run(arg).unwrap_err(), "validation failed for `Age`");
        // Assignment boundary (the binding's declared type is remembered).
        let assign = "type Age = Int64 where value >= 18 \
                      fn main() -> Int64 { let mut a: Age = 20 a = a - 15 return a }";
        assert_eq!(run(assign).unwrap_err(), "validation failed for `Age`");
        // Return boundary (a raw match join validates on the way out).
        let ret = "type Age = Int64 where value >= 18 \
                   fn pick(o: Option<Int64>) -> Age { \
                       return match o { Some(x) => x, None => 18 } } \
                   fn main() -> Int64 { return pick(Some(5)) }";
        assert_eq!(run(ret).unwrap_err(), "validation failed for `Age`");
        // Record-field boundary.
        let field = "type Age = Int64 where value >= 18 \
                     type User = { age: Age } \
                     fn mk(n: Int64) -> User { return User { age: n } } \
                     fn main() -> Int64 { let u = mk(5) return 0 }";
        assert_eq!(run(field).unwrap_err(), "validation failed for `Age`");
        // Cross-field record coercion (structural value into a predicated type).
        let xf = "type Range = { start: Int64, end: Int64 } where start < end \
                  type Plain = { start: Int64, end: Int64 } \
                  fn span(r: Range) -> Int64 { return r.end - r.start } \
                  fn mk(a: Int64, b: Int64) -> Plain { return Plain { start: a, end: b } } \
                  fn main() -> Int64 { return span(mk(9, 3)) }";
        assert_eq!(run(xf).unwrap_err(), "validation failed: `Range` violates its `where` clause");
    }

    #[test]
    fn inline_field_refinements_validate_like_named_types() {
        // Zod/ArkType-style inline `where` on fields: valid values flow through…
        let ok = "type User = { name: String where value.length >= 3, \
                                age: Int64 where value >= 18 } \
                  fn mk(n: Int64) -> User { return User { name: \"ada\", age: n } } \
                  fn main() -> Int64 { let u = mk(33) return u.age }";
        assert_eq!(run(ok).unwrap(), 33);
        // …a dynamic violation traps with the synthetic field-type name…
        let bad = "type User = { age: Int64 where value >= 18 } \
                   fn mk(n: Int64) -> User { return User { age: n } } \
                   fn main() -> Int64 { let u = mk(5) return 0 }";
        assert_eq!(run(bad).unwrap_err(), "validation failed for `User.age`");
        // …and a provably-bad constant is rejected at compile time.
        let constant = "type User = { age: Int64 where value >= 18 } \
                        fn main() -> Int64 { let u = User { age: 5 } return 0 }";
        assert!(run(constant).unwrap_err().contains("does not satisfy `User.age`"));
    }

    #[test]
    fn auto_validation_passes_valid_dynamic_values() {
        let src = "type Age = Int64 where value >= 18 \
                   fn g(a: Age) -> Int64 { return a } \
                   fn main() -> Int64 { \
                       let a: Age = 25 \
                       let mut m: Age = 21 \
                       m = m + 1 \
                       let xs: Array<Age, 2> = [19, 20] \
                       return g(a) + m + xs[1] }";
        assert_eq!(run(src).unwrap(), 25 + 22 + 20);
    }

    #[test]
    fn float_refined_type_constructs_and_rejects_at_runtime() {
        // Refinements over a Float base run under the runtime evaluator (this
        // used to fail for even VALID values — ConstVal had no Float).
        let ok = "type Ratio = Float64 where value > 0.0 && value <= 1.0; \
                  fn mk(x: Float64) -> Ratio { return Ratio(x); } \
                  fn main() -> Int64 { let r = mk(0.5); return 0; }";
        assert_eq!(run(ok).unwrap(), 0);
        let bad = "type Ratio = Float64 where value > 0.0 && value <= 1.0; \
                   fn mk(x: Float64) -> Ratio { return Ratio(x); } \
                   fn main() -> Int64 { let r = mk(2.5); return 0; }";
        assert!(run(bad).unwrap_err().contains("validation failed for `Ratio`"));
    }

    #[test]
    fn sized_int_refined_type_constructs_at_runtime() {
        let src = "type Small = Int32 where value < 100; \
                   fn mk(x: Int32) -> Small { return Small(x); } \
                   fn main() -> Int64 { let s = mk(Int32(5)); return 0; }";
        assert_eq!(run(src).unwrap(), 0);
    }

    #[test]
    fn cross_field_predicate_over_float_fields() {
        let ok = "type R = { a: Float64, b: Float64 } where a < b; \
                  fn mk(x: Float64, y: Float64) -> R { return R { a: x, b: y }; } \
                  fn main() -> Int64 { let r = mk(1.0, 2.0); return 0; }";
        assert_eq!(run(ok).unwrap(), 0);
        let bad = "type R = { a: Float64, b: Float64 } where a < b; \
                   fn mk(x: Float64, y: Float64) -> R { return R { a: x, b: y }; } \
                   fn main() -> Int64 { let r = mk(2.0, 1.0); return 0; }";
        assert!(run(bad).unwrap_err().contains("violates its `where`"));
    }

    #[test]
    fn int_arithmetic_wraps_like_native() {
        // i64::MAX + 1 wraps to i64::MIN in BOTH backends (and independent of
        // the cargo profile — bare `+` would panic in a debug build).
        let src = "fn main() -> Int64 { \
                       let m = 9223372036854775807 \
                       let w = m + 1 \
                       if w < 0 { return 1 } return 0 }";
        assert_eq!(run(src).unwrap(), 1);
        // -i64::MIN also wraps (back to MIN).
        let neg = "fn main() -> Int64 { \
                       let m = -9223372036854775808 \
                       let w = 0 - m \
                       if w < 0 { return 1 } return 0 }";
        assert_eq!(run(neg).unwrap(), 1);
    }

    #[test]
    fn division_traps_have_stable_messages() {
        let z = "fn main() -> Int64 { let mut d = 0; return 1 / d; }";
        assert_eq!(run(z).unwrap_err(), "division by zero");
        let rz = "fn main() -> Int64 { let mut d = 0; return 1 % d; }";
        assert_eq!(run(rz).unwrap_err(), "remainder by zero");
        // i64::MIN / -1 is unrepresentable: a clean trap, not a panic/SEH crash.
        let ovf = "fn main() -> Int64 { \
                       let m = -9223372036854775808 \
                       let mut d = 0 - 1 \
                       return m / d }";
        assert_eq!(run(ovf).unwrap_err(), "integer overflow in division");
    }

    #[test]
    fn wrapped_predicate_arithmetic_matches_native() {
        // `value + 1 != 0` at i64::MAX: wraps to MIN (≠ 0) — the predicate
        // holds in both backends (checked arithmetic used to refuse to prove
        // it and the interpreter then errored out).
        let src = "type T = Int64 where value + 1 != 0; \
                   fn mk(x: Int64) -> T { return T(x); } \
                   fn main() -> Int64 { let t = mk(9223372036854775807); return 0; }";
        assert_eq!(run(src).unwrap(), 0);
    }

    #[test]
    fn regex_match_operator() {
        let src = "fn main() -> Int64 { if \"abc\" =~ \"[a-z]+\" { return 1; } return 0; }";
        assert_eq!(run(src).unwrap(), 1);
        let no = "fn main() -> Int64 { if \"ab9\" =~ \"[a-z]+\" { return 1; } return 0; }";
        assert_eq!(run(no).unwrap(), 0);
    }

    #[test]
    fn validated_string_via_regex_traps() {
        let src = "type Code = String where value =~ \"[A-Z][A-Z][A-Z]\"; \
                   fn mk(s: String) -> Code { return Code(s); } \
                   fn main() -> Int64 { let c = mk(\"ab\"); return 0; }";
        assert!(run(src).unwrap_err().contains("validation failed for `Code`"));
    }

    #[test]
    fn validation_accumulates_all_issues() {
        // Both checks fail → Invalid carries both issues (i18n keys included).
        let src = "type P = { n: Int64 }; \
                   fn v(a: Int64, b: Int64) -> Validation<P> { \
                       let mut issues: Array<Issue> = []; \
                       if a < 0 { issues.push(Issue { key: \"a.min\", path: \"a\", message: \"m\" }); } \
                       if b < 0 { issues.push(Issue { key: \"b.min\", path: \"b\", message: \"m\" }); } \
                       if issues.length > 0 { return Invalid(issues); } \
                       return Valid(P { n: a + b }); } \
                   fn iss(x: Validation<P>) -> Array<Issue> { \
                       return match x { Valid(p) => [], Invalid(is) => is }; } \
                   fn main() -> Int64 { return iss(v(0 - 1, 0 - 1)).length; }";
        assert_eq!(run(src).unwrap(), 2);
    }

    #[test]
    fn validation_valid_case_carries_the_value() {
        let src = "type P = { n: Int64 }; \
                   fn v(a: Int64) -> Validation<P> { \
                       if a < 0 { return Invalid([]); } return Valid(P { n: a }); } \
                   fn valueOr(x: Validation<P>) -> Int64 { \
                       return match x { Valid(p) => p.n, Invalid(is) => 0 - 1 }; } \
                   fn main() -> Int64 { return valueOr(v(41)); }";
        assert_eq!(run(src).unwrap(), 41);
    }

    #[test]
    fn multiline_string_includes_the_newline() {
        // A raw newline inside "..." is part of the string (RFC-0007).
        let src = "fn main() -> Int64 { let s = \"ab\ncd\"; return s.length; }"; // 'a','b','\n','c','d' = 5
        assert_eq!(run(src).unwrap(), 5);
    }

    #[test]
    fn template_value_exposes_parts_and_values() {
        // `template"..."` yields a first-class Template { parts, values }.
        let src = "fn main() -> Int64 { let n = 7; let t = template\"a\\{n}b\"; \
                   return t.parts.length + t.values.length; }"; // 2 parts + 1 value = 3
        assert_eq!(run(src).unwrap(), 3);
    }

    #[test]
    fn tagged_template_needs_an_interpolation() {
        // A tag on a hole-less string is rejected (use a plain string instead).
        let src = "fn sql(p: Array<String>, v: Array<Value>) -> Int64 { return 0; } \
                   fn main() -> Int64 { return sql\"no holes here\"; }";
        assert!(run(src).unwrap_err().contains("interpolation"));
    }

    #[test]
    fn value_boxes_string_and_int_distinctly() {
        let src = "fn main() -> Int64 { \
                   let a = match value(7) { IntVal(n) => n, BoolVal(b) => 0, StrVal(s) => 0 - 1 }; \
                   let b = match value(\"hey\") { IntVal(n) => 0, BoolVal(x) => 0, StrVal(s) => s.length }; \
                   return a + b; }"; // 7 + 3
        assert_eq!(run(src).unwrap(), 10);
    }

    #[test]
    fn logger_and_levels_typecheck_and_run() {
        // A logger with each level, using interpolation in the message. Logs go
        // to stderr; the program returns normally.
        let src = "fn main() -> Int64 { let log = logger(\"t\"); let n = 2; \
                   log.trace(\"a\"); log.debug(\"b\"); log.info(\"n=\\{n}\"); \
                   log.warn(\"c\"); log.error(\"d\"); return n; }";
        assert_eq!(run(src).unwrap(), 2);
    }

    #[test]
    fn log_level_requires_a_logger() {
        // Calling a level on a non-Logger is rejected.
        let src = "fn main() -> Int64 { info(\"notalogger\", \"x\"); return 0; }";
        assert!(run(src).is_err());
    }

    #[test]
    fn logging_is_forbidden_in_spawned_tasks() {
        // A spawned function must be pure; logging is observable I/O.
        let src = "fn work(n: Int64) -> Int64 { let l = logger(\"w\"); l.info(\"hi\"); return n; } \
                   fn main() -> Int64 { let t = spawn work(1); return t.join(); }";
        assert!(run(src).is_err());
    }

    #[test]
    fn logging_config_block_parses_and_runs() {
        let src = "logging { level: warn } \
                   fn main() -> Int64 { let log = logger(\"a\"); \
                   log.info(\"filtered\"); log.error(\"shown\"); return 0; }";
        assert_eq!(run(src).unwrap(), 0);
    }

    #[test]
    fn invalid_log_level_is_rejected() {
        let src = "logging { level: loud } fn main() -> Int64 { return 0; }";
        assert!(run(src).unwrap_err().contains("log level"));
    }

    #[test]
    fn duplicate_logging_block_is_rejected() {
        let src = "logging { level: info } logging { level: warn } \
                   fn main() -> Int64 { return 0; }";
        assert!(run(src).unwrap_err().contains("duplicate"));
    }

    #[test]
    fn logging_sink_and_level_parse_together() {
        let src = "logging { level: warn, sink: stdout } \
                   fn main() -> Int64 { let l = logger(\"a\"); l.warn(\"x\"); return 0; }";
        assert_eq!(run(src).unwrap(), 0);
    }

    #[test]
    fn unknown_sink_is_rejected() {
        let src = "logging { sink: syslog } fn main() -> Int64 { return 0; }";
        assert!(run(src).unwrap_err().contains("sink"));
    }

    #[test]
    fn file_sink_needs_a_string_path() {
        let src = "logging { sink: file(main) } fn main() -> Int64 { return 0; }";
        assert!(run(src).is_err());
    }

    #[test]
    fn recursive_release_reclaims_the_slab() {
        // Build+free a list many more times than the cell budget: only possible
        // if `freeList` reclaims each node and its slot is reused.
        let src = "
            type Node = { value: Int64, next: Option<Ref<Node>> };
            fn freeList(o: Option<Ref<Node>>) -> Int64 {
                return match o { Some(r) => freeNode(r), None => 0 };
            }
            fn freeNode(r: Ref<Node>) -> Int64 {
                let tail = get(r).next; release(r); return freeList(tail);
            }
            fn main() -> Int64 {
                let mut i = 0;
                while i < 200 {
                    let mut head: Option<Ref<Node>> = None;
                    let mut j = 3;
                    while j > 0 { head = Some(cell(Node { value: j, next: head })); j = j - 1; }
                    freeList(head);
                    i = i + 1;
                }
                return 7;
            }
        ";
        assert_eq!(run(src).unwrap(), 7);
    }

    #[test]
    fn binary_tree_sum() {
        let src = "
            type Tree = { value: Int64, left: Option<Ref<Tree>>, right: Option<Ref<Tree>> };
            fn tsum(o: Option<Ref<Tree>>) -> Int64 {
                return match o {
                    Some(r) => get(r).value + tsum(get(r).left) + tsum(get(r).right),
                    None => 0,
                };
            }
            fn leaf(v: Int64) -> Option<Ref<Tree>> {
                return Some(cell(Tree { value: v, left: None, right: None }));
            }
            fn main() -> Int64 {
                let root = Some(cell(Tree { value: 2, left: leaf(1), right: leaf(4) }));
                return tsum(root);
            }
        ";
        assert_eq!(run(src).unwrap(), 7);
    }

    #[test]
    fn generic_reference_holds_any_type() {
        // A Ref<String> mutated in place, then measured.
        let src = "fn main() -> Int64 { let s = cell(\"ab\"); \
                       set(s, get(s) + \"cd\"); \
                       let n = get(s).length; release(s); return n; }";
        assert_eq!(run(src).unwrap(), 4);
    }

    #[test]
    fn use_after_release_is_caught() {
        // Access through a stale alias must fail the generation check, not dangle.
        let src = "fn main() -> Int64 { \
                       let c = cell(10); let d = c; release(c); return get(d); }";
        let e = run(src).unwrap_err();
        assert!(e.contains("used after release"), "{e}");
    }

    #[test]
    fn released_slot_is_reused_with_a_new_generation() {
        // After release, a fresh cell reuses the slot; the old reference is stale.
        let ok = "fn main() -> Int64 { \
                      let c = cell(1); release(c); let d = cell(2); return get(d); }";
        assert_eq!(run(ok).unwrap(), 2);
        let stale = "fn main() -> Int64 { \
                        let c = cell(1); release(c); let d = cell(2); return get(c); }";
        assert!(run(stale).unwrap_err().contains("used after release"));
    }

    #[test]
    fn spawn_and_join_fork_join() {
        let src = "
            fn sq(n: Int64) -> Int64 { return n * n; }
            fn main() -> Int64 {
                let a = spawn sq(6);
                let b = spawn sq(8);
                return a.join() + b.join();   // 36 + 64
            }
        ";
        assert_eq!(run(src).unwrap(), 100);
    }

    #[test]
    fn modify_parameter_writes_back_to_caller() {
        let src = "
            type C = { x: Int64 };
            fn bump(c: modify C) { c.x = c.x + 1; }
            fn main() -> Int64 {
                let mut c = C { x: 40 };
                bump(c); bump(c);   // caller's c is mutated each time
                return c.x;          // 42
            }
        ";
        assert_eq!(run(src).unwrap(), 42);
    }

    #[test]
    fn record_field_access_and_subtyping() {
        let src = "
            type Named = { name: Int64 };
            type Pt = { name: Int64, x: Int64, y: Int64 };
            fn nm(w: Named) -> Int64 { return w.name; }
            fn main() -> Int64 {
                let p = Pt { name: 3, x: 10, y: 20 };
                return nm(p) + p.x + p.y;   // 3 + 10 + 20
            }
        ";
        assert_eq!(run(src).unwrap(), 33);
    }

    #[test]
    fn enum_construct_and_match() {
        let src = "
            type Shape = | Circle(Int64) | Square(Int64) | Nil;
            fn area(s: Shape) -> Int64 {
                return match s { Circle(r) => 3 * r * r, Square(w) => w * w, Nil => 0 };
            }
            fn main() -> Int64 { return area(Circle(2)) + area(Square(5)) + area(Nil); }
        ";
        assert_eq!(run(src).unwrap(), 37); // 12 + 25 + 0
    }

    #[test]
    fn dynamic_string_concat_and_len() {
        let src = "fn g(n: String) -> String { return \"Hi, \" + n + \"!\"; } \
                   fn main() -> Int64 { return g(\"Vela\").length; }";
        assert_eq!(run(src).unwrap(), 9); // "Hi, Vela!" = 9 bytes
    }

    #[test]
    fn to_string_method_renders() {
        // `x.toString()` renders scalars, then `+` concatenates: "42/true" = 7.
        let src = "fn main() -> Int64 { let s = (42).toString() + \"/\" + true.toString(); \
                   return s.length; }";
        assert_eq!(run(src).unwrap(), 7);
    }

    #[test]
    fn contextual_array_literal_is_growable() {
        // A literal in an `Array<T>` position is a growable heap array you can
        // `push` onto — its element count is observable via `.length`.
        let src = "fn main() -> Int64 { let mut a: Array<Int64> = [1, 2, 3]; \
                   a.push(4); return a.length + a[3]; }"; // 4 + 4
        assert_eq!(run(src).unwrap(), 8);
    }

    #[test]
    fn task_join_method_awaits_result() {
        let src = "fn sq(n: Int64) -> Int64 { return n * n } \
                   fn main() -> Int64 { let t = spawn sq(9); return t.join() }";
        assert_eq!(run(src).unwrap(), 81);
    }

    #[test]
    fn string_eq() {
        let src = "fn main() -> Int64 { \
                   let s = \"hello\"; \
                   if s == \"hello\" { return 1; } return 0; }";
        assert_eq!(run(src).unwrap(), 1);
    }

    #[test]
    fn while_loop_and_mut() {
        let src = "
            fn main() -> Int64 {
                let mut i = 0;
                let mut sum = 0;
                while i < 5 {
                    sum = sum + i;
                    i = i + 1;
                }
                return sum;
            }
        ";
        assert_eq!(run(src).unwrap(), 10); // 0+1+2+3+4
    }

    /// Calling an `extern fn` (RFC-0012) traps: the interpreter has no host to
    /// provide it. Declaring one is fine — only the call is the unavailable
    /// effect. Wording is byte-identical to the native trap stub's.
    #[test]
    fn extern_call_traps_with_canonical_wording() {
        let src = "extern fn jsNow() -> Float64\n\
                   fn main() -> Int64 {\n\
                       let t = jsNow()\n\
                       return 0\n\
                   }";
        assert_eq!(run(src).unwrap_err(), "extern `jsNow` is not available on this target");
        // Declaring without calling is harmless.
        let src = "extern fn jsNow() -> Float64\nfn main() -> Int64 { return 7 }";
        assert_eq!(run(src).unwrap(), 7);
    }

    /// An `export extern fn` (RFC-0012 M2) is a normal function: calling it from
    /// Vela runs its body — no trap anywhere. Only body-less imports trap
    /// off-wasm, so an export-extern-using program stays three-way-parity-capable.
    #[test]
    fn export_extern_is_a_normal_call() {
        let src = "export extern fn velaAdd(a: Int64, b: Int64) -> Int64 { return a + b }\n\
                   fn main() -> Int64 { return velaAdd(40, 2) }";
        assert_eq!(run(src).unwrap(), 42);
    }

    /// The native arena runtime has a fixed 64-slot region stack and traps on
    /// a 65th nested region; the interpreter enforces the identical bound with
    /// the identical message — depth accumulates dynamically across calls.
    #[test]
    fn region_nesting_is_bounded_at_64() {
        let src = |n: i64| {
            format!(
                "fn deep(n: Int64) -> Int64 {{
                     if n == 0 {{ return 0; }}
                     region {{
                         return deep(n - 1);
                     }}
                 }}
                 fn main() -> Int64 {{ return deep({n}); }}"
            )
        };
        // 64 nested regions fill the stack exactly — fine.
        assert_eq!(run(&src(64)).unwrap(), 0);
        // The 65th traps, wording shared with the native runtime.
        assert_eq!(run(&src(65)).unwrap_err(), "region nesting exceeds 64");
    }

    // ---- in-place array mutation (RFC-0011) -----------------------------

    #[test]
    fn index_store_mutates_in_place() {
        let src = "fn main() -> Int64 { let mut a: Array<Int64> = [10, 20, 30]; \
                   a[1] = 25; return a[0] + a[1] + a[2]; }";
        assert_eq!(run(src).unwrap(), 65);
    }

    #[test]
    fn index_store_out_of_bounds_traps() {
        let src = "fn main() -> Int64 { let mut a: Array<Int64> = [1, 2, 3]; a[5] = 9; return 0; }";
        assert_eq!(run(src).unwrap_err(), "array index 5 out of bounds");
    }

    #[test]
    fn pop_returns_last_and_shrinks() {
        let src = "fn main() -> Int64 { let mut a: Array<Int64> = [1, 2, 7]; \
                   let p = match a.pop() { Some(x) => x, None => -1 }; \
                   return p * 100 + a.length; }";
        assert_eq!(run(src).unwrap(), 702); // popped 7, length now 2
    }

    #[test]
    fn pop_on_empty_is_none() {
        let src = "fn main() -> Int64 { let mut a: Array<Int64> = [5]; \
                   let p1 = a.pop(); let p2 = a.pop(); \
                   return match p2 { Some(x) => x, None => -1 }; }";
        assert_eq!(run(src).unwrap(), -1);
    }

    #[test]
    fn swapremove_moves_last_into_slot() {
        // [10, 20, 30, 40]; swapRemove(1) returns 20, moves 40 into slot 1.
        let src = "fn main() -> Int64 { let mut a: Array<Int64> = [10, 20, 30, 40]; \
                   let g = a.swapRemove(1); \
                   return g * 1000 + a[0] * 100 + a[1] + a.length; }";
        // g=20 -> 20000; a=[10,40,30]; 10*100=1000; a[1]=40; length=3 -> 21043
        assert_eq!(run(src).unwrap(), 21043);
    }

    #[test]
    fn swapremove_out_of_bounds_traps() {
        let src = "fn main() -> Int64 { let mut a: Array<Int64> = [1, 2, 3]; \
                   let g = a.swapRemove(9); return g; }";
        assert_eq!(run(src).unwrap_err(), "array index 9 out of bounds");
    }

    #[test]
    fn index_store_validated_element_traps_at_runtime() {
        let src = "type Age = Int64 where value >= 18 \
                   fn main() -> Int64 { let mut a: Array<Age> = [Age(20)]; \
                   let mut n = 5; a[0] = n; return 0; }";
        assert_eq!(run(src).unwrap_err(), "validation failed for `Age`");
    }
}
