//! The wasm module encoder (RFC-0077 M1): the scaffolding a lowered function
//! body gets written into.
//!
//! This is the half of the direct backend that has no opinions about Vyrn — it
//! frames sections, interns data, hands out function indices, and puts the
//! shadow-stack prologue and epilogue around a body someone else emits. M2 fills
//! the bodies in.
//!
//! Three things here are not free choices, and are the reason the module exists
//! at all rather than being inlined into the emitter later:
//!
//! **Section order is normative.** wasm's binary format fixes the order of its
//! sections, and [`wasm_encoder`] will cheerfully emit them in whatever order it
//! is called in. So sections are accumulated as fields and framed once, in
//! [`Module::finish`], where the order is written down in one place instead of
//! being an emergent property of the traversal.
//!
//! **The memory map is measured** (RFC-0077 M0), not chosen: a 64 KB shadow
//! stack growing DOWN from [`STACK_TOP`], data segments from [`DATA_BASE`] up,
//! statics ending below [`STATICS_LIMIT`]. That is what `--stack-first` gives
//! today and what the RFC-0076 shim's own placement leaves room for, so a
//! directly-emitted module reproducing it needs no negotiation with either.
//! Overflow is load-bearing: a frame push past address 0 wraps to `0xFFFFFFF8`
//! and the first access traps, rather than quietly walking into the data
//! segments. `tests/wasm_runs.rs` runs that off the end and checks the trap.
//!
//! **`i1` does not exist in wasm.** LLVM widens `i1` to `i32` at the C boundary
//! silently — which is how `declare ptr @__vyrn_vj_bool(i1)` has been calling
//! `VJ* __vyrn_vj_bool(int)` correctly all along. A direct emitter gets no such
//! favour, so [`abi`] widens, and `tests/imports_vs_shim.rs` checks the widened
//! signatures against the C the shim actually defines.

use std::collections::{BTreeMap, HashMap};
use wasm_encoder::{
    CodeSection, ConstExpr, DataSection, EntityType, ExportKind, ExportSection, Function,
    FunctionSection, GlobalSection, GlobalType, ImportSection, MemorySection, MemoryType,
    TypeSection,
};
pub use wasm_encoder::{BlockType, Instruction, MemArg, ValType};

/// Top of the generated module's shadow stack; it grows down from here to 0.
pub const STACK_TOP: u32 = 65_536;
/// First byte of the generated module's data segments. Statics grow up from
/// here, so the stack below can only reach them by underflowing past 0 — which
/// traps.
pub const DATA_BASE: u32 = 65_536;
/// Where the RFC-0076 shim's data and heap begin, and where its stack starts
/// growing back down. The one address the two modules have to agree about, so it
/// is written down once: [`crate::toolchain::shim_wasm`] passes it to
/// `--global-base`/`-z stack-size`, and `STATICS_LIMIT` is derived from it.
pub const SHIM_BASE: u32 = 16 * 1024 * 1024;

/// Everything this module statically occupies must end below here: half of
/// [`SHIM_BASE`], the gap that keeps the shim's downward-growing frames from ever
/// reaching our data. `compile_split` checks the same number on the linked bytes
/// today; a direct emitter knows it before it writes it.
pub const STATICS_LIMIT: u32 = SHIM_BASE / 2;

/// wasm has no sub-32-bit values on the operand stack, and the stack pointer
/// itself is 4-byte-aligned by convention; clang keeps frames 16-aligned on
/// wasm32 and matching it costs nothing.
const FRAME_ALIGN: u32 = 16;

/// The wasm value type an LLVM type crosses a call boundary as, or `None` for
/// `void`.
///
/// Two widenings, both of them LLVM fictions that wasm does not have:
/// `i1`/`i8`/`i16` are `i32`, and a `ptr` is an `i32` address on wasm32. An
/// aggregate is also an `i32` — the address of its shadow-stack slot — which is
/// the whole aggregate ABI in one line. M0 measured that no aggregate actually
/// crosses the C boundary by value (0 of 75 externals), so that case only ever
/// arises on Vyrn-internal calls, where the convention is ours to pick.
pub fn abi(ll: &str) -> Option<ValType> {
    Some(match ll.trim() {
        "void" => return None,
        "double" => ValType::F64,
        "float" => ValType::F32,
        "i64" => ValType::I64,
        _ => ValType::I32,
    })
}

/// One boundary signature: the wasm type of each parameter and of the result,
/// `None` where the C says `void`.
pub type Sig = (Vec<Option<ValType>>, Option<ValType>);

/// Every function the textual emitter `declare`s, as the wasm signature it
/// crosses as — `None` for the three variadic ones, which wasm cannot express at
/// all (RFC-0077 M3).
///
/// Read off the `declare` lines rather than written down, because those lines are
/// the side `tests/imports_vs_shim.rs` proves agrees with the C the shim defines.
/// A second list here would be a second chance to get a signature wrong, and a
/// wrong import signature is not a link error — it is a misread argument. The
/// boundary declarations are unconditional, which is what makes one trivial
/// program a complete census of them.
pub fn boundary() -> &'static BTreeMap<String, Option<Sig>> {
    static ONCE: std::sync::OnceLock<BTreeMap<String, Option<Sig>>> = std::sync::OnceLock::new();
    ONCE.get_or_init(|| {
        let toks = vyrn_frontend::lexer::lex("fn main() -> Int64 { return 0 }").expect("lex");
        let program = vyrn_frontend::parser::parse(toks).expect("parse");
        let ir = crate::emit(&program).expect("the boundary declarations are unconditional");
        let mut out = BTreeMap::new();
        for line in ir.lines() {
            let Some(rest) = line.strip_prefix("declare ") else { continue };
            let (ret, rest) = rest.split_once(" @").expect("declare RET @NAME(..)");
            let (name, rest) = rest.split_once('(').expect("declare RET @NAME(..)");
            // `llvm.memcpy` is an intrinsic, not an import: it becomes `memory.copy`.
            if name.starts_with("llvm.") {
                continue;
            }
            let args = rest.rsplit_once(')').expect("declare RET @NAME(..)").0;
            let sig = if args.contains("...") {
                None
            } else {
                Some((split_args(args).iter().map(|a| abi(a)).collect(), abi(ret)))
            };
            out.insert(name.to_string(), sig);
        }
        out
    })
}

/// Split a parameter list on top-level commas. Nothing in the boundary nests
/// today — M0 measured no aggregate crossing it — but a `void (*)(ptr, i64)`
/// would, and one comma read wrong shifts every parameter after it.
fn split_args(s: &str) -> Vec<String> {
    let (mut out, mut depth, mut start) = (Vec::new(), 0i32, 0usize);
    for (i, c) in s.char_indices() {
        match c {
            '(' | '{' | '[' => depth += 1,
            ')' | '}' | ']' => depth -= 1,
            ',' if depth == 0 => {
                out.push(s[start..i].trim().to_string());
                start = i + 1;
            }
            _ => {}
        }
    }
    if !s.trim().is_empty() {
        out.push(s[start..].trim().to_string());
    }
    out
}

/// A module under construction.
pub struct Module {
    types: TypeSection,
    /// Deduplicates signatures — a module with 6,000 functions has a few dozen
    /// distinct types, and the type section is scanned linearly by validators.
    type_ids: HashMap<(Vec<ValType>, Vec<ValType>), u32>,
    imports: ImportSection,
    import_memory: bool,
    n_imports: u32,
    funcs: FunctionSection,
    code: CodeSection,
    exports: ExportSection,
    /// The single data segment, packed at [`DATA_BASE`]; `pool_at` deduplicates
    /// identical contents, which is what makes it a string pool.
    pool: Vec<u8>,
    pool_at: HashMap<Vec<u8>, u32>,
}

impl Default for Module {
    fn default() -> Self {
        Self::new()
    }
}

impl Module {
    /// A standalone module: it defines and exports its own memory, the shape
    /// `vyrn build --target wasm` produces.
    pub fn new() -> Self {
        Module {
            types: TypeSection::new(),
            type_ids: HashMap::new(),
            imports: ImportSection::new(),
            import_memory: false,
            n_imports: 0,
            funcs: FunctionSection::new(),
            code: CodeSection::new(),
            exports: ExportSection::new(),
            pool: Vec::new(),
            pool_at: HashMap::new(),
        }
    }

    /// Take memory from the shared RFC-0076 shim instead of defining one. The
    /// two modules then live in one address space, which is the point — one
    /// `malloc` heap, no copying across the boundary.
    pub fn import_memory(&mut self) -> &mut Self {
        self.import_memory = true;
        self
    }

    /// Intern a signature, giving its type index.
    fn ty(&mut self, params: &[ValType], results: &[ValType]) -> u32 {
        let key = (params.to_vec(), results.to_vec());
        if let Some(&i) = self.type_ids.get(&key) {
            return i;
        }
        let i = self.types.len();
        self.types.ty().function(params.iter().copied(), results.iter().copied());
        self.type_ids.insert(key, i);
        i
    }

    /// Declare an imported function, giving its index.
    ///
    /// Imported functions occupy the bottom of the same index space as defined
    /// ones, so every import has to be declared before the first [`Module::func`]
    /// — a rule worth panicking on rather than debugging as an off-by-N in every
    /// call in the module.
    pub fn import(&mut self, module: &str, field: &str, params: &[ValType], results: &[ValType]) -> u32 {
        assert!(
            self.funcs.is_empty(),
            "import {module}.{field} declared after a defined function: \
             imports and definitions share one index space, imports first"
        );
        let t = self.ty(params, results);
        self.imports.import(module, field, EntityType::Function(t));
        self.n_imports += 1;
        self.n_imports - 1
    }

    /// Place `bytes` in the data segment at an `align`-aligned address, giving
    /// that address. Identical contents at the same alignment are shared.
    pub fn data(&mut self, bytes: &[u8], align: u32) -> u32 {
        debug_assert!(align.is_power_of_two());
        if let Some(&at) = self.pool_at.get(bytes) {
            if at % align == 0 {
                return at;
            }
        }
        let at = DATA_BASE + round_up(self.pool.len() as u32, align);
        self.pool.resize((at - DATA_BASE) as usize, 0);
        self.pool.extend_from_slice(bytes);
        self.pool_at.insert(bytes.to_vec(), at);
        at
    }

    /// Reserve `size` zero bytes at an `align`-aligned address, giving that
    /// address. Storage rather than a constant: module state (RFC-0013) lives
    /// here, and every reservation is its own address.
    ///
    /// Deliberately NOT [`Module::data`], which shares identical contents — two
    /// zero-initialized globals of the same size would be one address, and they
    /// would be the same variable.
    pub fn reserve(&mut self, size: u32, align: u32) -> u32 {
        debug_assert!(align.is_power_of_two());
        let at = DATA_BASE + round_up(self.pool.len() as u32, align);
        self.pool.resize((at - DATA_BASE + size) as usize, 0);
        at
    }

    /// The first address past everything this module statically occupies. The
    /// number [`STATICS_LIMIT`] bounds.
    pub fn data_end(&self) -> u32 {
        DATA_BASE + self.pool.len() as u32
    }

    /// Define a function, giving its index.
    ///
    /// `frame` is a reservation in bytes at the bottom of its shadow-stack frame;
    /// a body grows the rest with [`Frame::alloc`]. Frames are statically sized
    /// either way (M0: 17,270 allocas, none of them dynamically sized) — the size
    /// is simply not known until the body has been walked, which is why the body
    /// is BUFFERED here and the prologue written in front of it afterwards. The
    /// alternative was a sizing pre-pass over the AST, i.e. a second traversal
    /// that has to agree with the first about which expressions need a slot.
    ///
    /// Locals are dynamic for the same reason: `locals` are pre-declared (a body
    /// that wants fixed indices), and [`Frame::local`] appends after them. So
    /// indices are parameters `0..params.len()`, then [`Frame::base`] —
    /// allocated even for an empty frame, so the numbering does not depend on
    /// whether a function happens to need stack — then `locals`, then whatever
    /// the body asked for.
    pub fn func(
        &mut self,
        params: &[ValType],
        results: &[ValType],
        locals: &[ValType],
        frame: u32,
        build: impl FnOnce(&mut Frame),
    ) -> u32 {
        let mut f = Frame::new(params.len(), locals, frame);
        build(&mut f);
        self.add(params, results, f)
    }

    /// Install an already-built body as a function, giving its index.
    ///
    /// The half of [`Module::func`] that a lowering wants when it needs the
    /// module WHILE it emits — interning a string literal mid-expression, say,
    /// which a `build` closure borrowing `&mut Module` could not do.
    pub fn add(&mut self, params: &[ValType], results: &[ValType], f: Frame) -> u32 {
        let t = self.ty(params, results);
        self.funcs.function(t);
        debug_assert_eq!(f.base, params.len() as u32, "frame built for a different signature");

        let mut decl = vec![ValType::I32]; // the frame base
        decl.extend(f.locals.iter().copied());
        let mut out = Function::new_with_locals_types(decl);
        let frame = round_up(f.frame, FRAME_ALIGN);
        // Claim the frame. Subtracting past 0 wraps to near `0xFFFFFFFF`, where
        // every access is out of bounds — the trap `--stack-first` buys, and the
        // reason the stack is at the BOTTOM of memory rather than above the data
        // it would otherwise overwrite.
        if frame != 0 {
            out.instruction(&Instruction::GlobalGet(SP))
                .instruction(&Instruction::I32Const(frame as i32))
                .instruction(&Instruction::I32Sub)
                .instruction(&Instruction::LocalTee(f.base))
                .instruction(&Instruction::GlobalSet(SP));
        }
        for i in &f.body {
            out.instruction(i);
        }
        // Release it: adding back to the base is the same value the prologue
        // found, without a second local to hold it.
        if frame != 0 {
            out.instruction(&Instruction::LocalGet(f.base))
                .instruction(&Instruction::I32Const(frame as i32))
                .instruction(&Instruction::I32Add)
                .instruction(&Instruction::GlobalSet(SP));
        }
        out.instruction(&Instruction::End);
        self.code.function(&out);
        self.n_imports + self.funcs.len() - 1
    }

    /// How many functions this module imports — the offset a defined
    /// function's index starts from, which a lowering needs before any body
    /// exists in order to name a callee it has not emitted yet.
    pub fn n_imports(&self) -> u32 {
        self.n_imports
    }

    /// The index the next defined function will get.
    ///
    /// Emission order IS the numbering — indices are baked into every `call` — so
    /// a lowering that reserved indices ahead of the bodies can check here that
    /// the bodies arrived where it said they would.
    pub fn next_func(&self) -> u32 {
        self.n_imports + self.funcs.len()
    }

    /// Export a defined function under `name` — `vyrn_entry`, and one per
    /// RFC-0012 `export extern fn` under its own `wasm-export-name`.
    pub fn export(&mut self, name: &str, func: u32) -> &mut Self {
        self.exports.export(name, ExportKind::Func, func);
        self
    }

    /// The finished bytes.
    ///
    /// Sections go out in the order the format fixes: type, import, function,
    /// memory, global, export, code, data. Nothing else in this file may emit a
    /// section, so this list is the whole ordering constraint.
    pub fn finish(mut self) -> Vec<u8> {
        assert!(
            self.data_end() <= STATICS_LIMIT,
            "statics end at {} — past the {STATICS_LIMIT}-byte line the shim's stack needs",
            self.data_end()
        );
        let mem = MemoryType {
            // One page past the top of everything we occupy; wasi-libc's
            // `malloc` grows memory itself from there.
            minimum: (round_up(self.data_end(), 65_536) / 65_536) as u64,
            maximum: None,
            memory64: false,
            shared: false,
            page_size_log2: None,
        };
        let mut memories = MemorySection::new();
        if self.import_memory {
            // The import section is already framed by index above; memory is an
            // import too, and imports of different kinds may interleave.
            self.imports.import("env", "memory", EntityType::Memory(mem));
        } else {
            memories.memory(mem);
        }
        // Exported either way, and the imported case is not cosmetic: wasmtime's
        // WASI has to read an iovec out of the MAIN module's memory, so a module
        // that only imports one fails `fd_write` with "missing required memory
        // export" at the first `print`. An imported memory is index 0 too, so
        // re-exporting it is the whole fix.
        self.exports.export("memory", ExportKind::Memory, 0);

        let mut globals = GlobalSection::new();
        globals.global(
            GlobalType { val_type: ValType::I32, mutable: true, shared: false },
            &ConstExpr::i32_const(STACK_TOP as i32),
        );
        // The bump heap starts where the statics end, 16-aligned so a `malloc`
        // result is aligned for anything `layout` can put in it. This is the
        // number M0 said a direct emitter would know by construction instead of
        // reading back out of linked bytes.
        globals.global(
            GlobalType { val_type: ValType::I32, mutable: true, shared: false },
            &ConstExpr::i32_const(round_up(self.data_end(), 16) as i32),
        );

        let mut data = DataSection::new();
        if !self.pool.is_empty() {
            data.active(0, &ConstExpr::i32_const(DATA_BASE as i32), self.pool.iter().copied());
        }

        let mut m = wasm_encoder::Module::new();
        m.section(&self.types);
        if !self.imports.is_empty() {
            m.section(&self.imports);
        }
        m.section(&self.funcs);
        if !self.import_memory {
            m.section(&memories);
        }
        m.section(&globals);
        m.section(&self.exports);
        m.section(&self.code);
        if !data.is_empty() {
            m.section(&data);
        }
        m.finish()
    }
}

/// The module's `__stack_pointer`. Index 0 because the encoder defines its
/// globals before anything else can — the emitter has no module-level mutable
/// state of its own that is not in memory.
pub const SP: u32 = 0;

/// The module's bump-allocation cursor, initialized past the statics by
/// [`Module::finish`]. RFC-0076's shim owns `malloc` for the split build; a
/// standalone module has no shim to ask, so the direct backend emits its own
/// against this global.
pub const HEAP: u32 = 1;

/// A function body under construction, plus the frame it is accumulating.
///
/// The instructions are buffered rather than encoded as they arrive, because the
/// prologue in front of them depends on a frame size only the finished body
/// knows. See [`Module::func`].
pub struct Frame {
    body: Vec<Instruction<'static>>,
    locals: Vec<ValType>,
    next_local: u32,
    frame: u32,
    /// Local holding the frame's base address, valid for the whole body.
    base: u32,
}

impl Frame {
    /// An empty body for a function with `n_params` parameters, `locals`
    /// pre-declared after the frame base, and `frame` bytes of stack reserved
    /// before anything [`Frame::alloc`] adds.
    pub fn new(n_params: usize, locals: &[ValType], frame: u32) -> Self {
        let base = n_params as u32;
        Frame {
            body: Vec::new(),
            locals: locals.to_vec(),
            next_local: base + 1 + locals.len() as u32,
            frame,
            base,
        }
    }

    /// Append one instruction.
    ///
    /// M1 note carried into M2: a body must not emit `return`, because it would
    /// jump past the epilogue and leak the frame. Returns go through a `br` to
    /// the function's outermost block, or the epilogue moves into a helper the
    /// return path calls.
    pub fn ins(&mut self, i: &Instruction<'static>) -> &mut Self {
        self.body.push(i.clone());
        self
    }

    /// Take another local of type `t`, giving its index.
    pub fn local(&mut self, t: ValType) -> u32 {
        self.locals.push(t);
        self.next_local += 1;
        self.next_local - 1
    }

    /// Take `size` bytes of frame at `align`, giving the offset from the frame
    /// base. Offsets are handed out for the whole function, never reused — a
    /// slot inside a loop is one slot, written afresh each turn.
    pub fn alloc(&mut self, size: u32, align: u32) -> u32 {
        debug_assert!(align.is_power_of_two());
        let at = round_up(self.frame, align.max(1));
        self.frame = at + size;
        at
    }

    /// Push the address of the frame slot at `off`.
    pub fn slot(&mut self, off: u32) -> &mut Self {
        self.body.push(Instruction::LocalGet(self.base));
        if off != 0 {
            self.body.push(Instruction::I32Const(off as i32));
            self.body.push(Instruction::I32Add);
        }
        self
    }

    /// The local holding the frame base, for a body that wants to index it
    /// itself.
    pub fn base(&self) -> u32 {
        self.base
    }
}

fn round_up(n: u32, align: u32) -> u32 {
    (n + align - 1) & !(align - 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The section ids of `wasm`, in order, with a check that it is a module at
    /// all. Deliberately not a real parser: the point is to read back what we
    /// wrote without a dependency that could agree with us for the same reason.
    fn section_ids(wasm: &[u8]) -> Vec<u8> {
        assert_eq!(&wasm[..8], b"\0asm\x01\0\0\0", "not a wasm module");
        let (mut i, mut ids) = (8usize, Vec::new());
        while i < wasm.len() {
            ids.push(wasm[i]);
            i += 1;
            let (mut len, mut shift) = (0u32, 0);
            loop {
                let b = wasm[i];
                i += 1;
                len |= ((b & 0x7f) as u32) << shift;
                shift += 7;
                if b & 0x80 == 0 {
                    break;
                }
            }
            i += len as usize;
        }
        assert_eq!(i, wasm.len(), "a section ran off the end");
        ids
    }

    #[test]
    fn sections_come_out_in_the_order_the_format_fixes() {
        let mut m = Module::new();
        m.import("wasi_snapshot_preview1", "proc_exit", &[ValType::I32], &[]);
        m.data(b"hi\0", 1);
        let f = m.func(&[], &[ValType::I32], &[], 16, |b| {
            b.ins(&Instruction::I32Const(0));
        });
        m.export("vyrn_entry", f);
        let ids = section_ids(&m.finish());
        assert_eq!(ids, vec![1, 2, 3, 5, 6, 7, 10, 11]);
        assert!(ids.windows(2).all(|w| w[0] < w[1]), "sections out of order: {ids:?}");
    }

    /// An imported memory moves the memory section away but NOT the export —
    /// the RFC-0076 split shape, where the shim owns the address space and WASI
    /// still needs to find it on the main module.
    #[test]
    fn importing_memory_drops_the_memory_section_but_keeps_the_export() {
        let mut m = Module::new();
        m.import_memory();
        let f = m.func(&[], &[], &[], 0, |_| {});
        m.export("vyrn_entry", f);
        let bytes = m.finish();
        assert_eq!(section_ids(&bytes), vec![1, 2, 3, 6, 7, 10]);
        assert!(
            bytes.windows(6).any(|w| w == b"memory"),
            "an imported memory must still be exported, or WASI cannot read an iovec"
        );
    }

    /// The census is the emitter's own `declare` lines, so a signature here
    /// cannot disagree with the one the audit checks against the C.
    #[test]
    fn the_boundary_census_is_the_declare_lines() {
        let b = boundary();
        // The census used to lead with `__vyrn_vj_bool(i1)` — M0's one widening,
        // an LLVM `i1` crossing as an `i32`. RFC-0078 M2b retired the JSON DOM
        // BUILDERS along with the shim's serializer, and that was the boundary's
        // only sub-i32 argument, so the widening is now a fact about `abi`
        // (asserted in the next test) with no live crossing to point at. M3 took
        // the rest of the DOM, so the census has no JSON row at all — pinned as an
        // absence, since a returning row would mean a second reader appeared.
        assert!(!b.keys().any(|k| k.starts_with("__vyrn_vj_")), "the DOM is gone");
        assert_eq!(b["__vyrn_charcount"], Some((vec![Some(ValType::I32)], Some(ValType::I64))));
        assert_eq!(b["__vyrn_malloc"], Some((vec![Some(ValType::I64)], Some(ValType::I32))));
        assert_eq!(b["__vyrn_now_millis"], Some((vec![], Some(ValType::I64))));
        assert_eq!(b["free"], Some((vec![Some(ValType::I32)], None)), "void is None");
        assert_eq!(b["printf"], None, "variadic has no wasm signature at all");
        assert!(!b.contains_key("llvm.memcpy.p0.p0.i64"), "an intrinsic is not an import");
    }

    #[test]
    fn the_boundary_widens_what_wasm_does_not_have() {
        // The `vj_bool` case: an LLVM `i1` is an `i32` argument.
        assert_eq!(abi("i1"), Some(ValType::I32));
        assert_eq!(abi("i8"), Some(ValType::I32));
        assert_eq!(abi("i16"), Some(ValType::I32));
        assert_eq!(abi("i32"), Some(ValType::I32));
        assert_eq!(abi("ptr"), Some(ValType::I32));
        assert_eq!(abi("i64"), Some(ValType::I64));
        assert_eq!(abi("double"), Some(ValType::F64));
        assert_eq!(abi("float"), Some(ValType::F32));
        assert_eq!(abi("void"), None);
        // An aggregate is its shadow-stack address, so it is an i32 too.
        assert_eq!(abi("{ ptr, i64, i64 }"), Some(ValType::I32));
    }

    #[test]
    fn the_data_pool_packs_and_shares() {
        let mut m = Module::new();
        assert_eq!(m.data(b"hello\0", 1), DATA_BASE);
        assert_eq!(m.data(b"hello\0", 1), DATA_BASE, "identical contents are one string");
        assert_eq!(m.data(b"bye\0", 1), DATA_BASE + 6);
        // An aligned static skips the hole rather than landing mid-word.
        assert_eq!(m.data(&[0u8; 8], 8), DATA_BASE + 16);
        assert_eq!(m.data_end(), DATA_BASE + 24);
    }

    /// Two globals of the same size are two variables. `data` would have shared
    /// them, which is why module state does not go through it.
    #[test]
    fn a_reservation_is_never_shared_with_another() {
        let mut m = Module::new();
        let a = m.reserve(8, 8);
        let b = m.reserve(8, 8);
        assert_ne!(a, b);
        assert_eq!(b, a + 8);
        assert_eq!(m.data(b"x\0", 1), b + 8, "a later string packs after the reservation");
        // And a reservation reads as zeroes, not as whatever preceded it.
        assert_eq!(m.reserve(4, 4), b + 12);
    }

    /// Frames are rounded, and an empty one costs no instructions — but still
    /// takes its local, so a body's local numbering never depends on it.
    #[test]
    fn an_empty_frame_emits_no_prologue() {
        let mut m = Module::new();
        let empty = m.func(&[ValType::I32], &[], &[], 0, |b| {
            assert_eq!(b.base(), 1);
        });
        let framed = m.func(&[], &[], &[], 1, |_| {});
        // The prologue is five instructions; an empty frame emits none of them,
        // so the framed body is strictly longer than the one that took no stack.
        assert!(framed > empty);
    }

    /// A slot and a local are both handed out mid-body, which is the whole point
    /// of buffering: the frame size is not known until the body is walked.
    #[test]
    fn slots_and_locals_are_taken_as_the_body_needs_them() {
        let mut m = Module::new();
        m.func(&[ValType::I64], &[], &[ValType::I32], 0, |b| {
            // params 0, the frame base 1, the pre-declared local 2, then ours.
            assert_eq!(b.local(ValType::I64), 3);
            assert_eq!(b.local(ValType::I32), 4);
            assert_eq!(b.alloc(4, 4), 0);
            // The i64 skips the hole rather than landing mid-word.
            assert_eq!(b.alloc(8, 8), 8);
            assert_eq!(b.alloc(1, 1), 16);
        });
    }

    #[test]
    #[should_panic(expected = "imports and definitions share one index space")]
    fn an_import_after_a_definition_is_an_index_bug_waiting_to_happen() {
        let mut m = Module::new();
        m.func(&[], &[], &[], 0, |_| {});
        m.import("env", "late", &[], &[]);
    }

    #[test]
    #[should_panic(expected = "past the")]
    fn statics_may_not_cross_the_line_the_shim_needs() {
        let mut m = Module::new();
        m.data(&vec![0u8; STATICS_LIMIT as usize], 1);
        m.finish();
    }
}
