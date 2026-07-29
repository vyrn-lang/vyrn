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

use std::collections::HashMap;
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
/// Everything this module statically occupies must end below here: half of the
/// RFC-0076 shim's base (16 MB), the gap that keeps the shim's downward-growing
/// frames from ever reaching our data. `compile_split` checks the same number on
/// the linked bytes today; a direct emitter knows it before it writes it.
pub const STATICS_LIMIT: u32 = 8 * 1024 * 1024;

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

    /// The first address past everything this module statically occupies. The
    /// number [`STATICS_LIMIT`] bounds.
    pub fn data_end(&self) -> u32 {
        DATA_BASE + self.pool.len() as u32
    }

    /// Define a function, giving its index.
    ///
    /// `frame` is the size in bytes of its shadow-stack frame — statically known
    /// for every function the emitter produces (M0: 17,270 allocas, none of them
    /// dynamically sized). The prologue and epilogue that claim and release it
    /// are emitted here so no body has to remember to.
    ///
    /// Local indices: parameters are `0..params.len()`, then [`Frame::base`] —
    /// allocated even for an empty frame, so the numbering does not depend on
    /// whether a function happens to need stack — then `locals` after it.
    pub fn func(
        &mut self,
        params: &[ValType],
        results: &[ValType],
        locals: &[ValType],
        frame: u32,
        build: impl FnOnce(&mut Frame),
    ) -> u32 {
        let t = self.ty(params, results);
        self.funcs.function(t);
        let mut decl = vec![(1u32, ValType::I32)]; // the frame base
        decl.extend(locals.iter().map(|&l| (1u32, l)));
        let base = params.len() as u32;
        let frame = round_up(frame, FRAME_ALIGN);
        let mut f = Frame { f: Function::new(decl), frame, base };
        f.prologue();
        build(&mut f);
        f.epilogue();
        f.f.instruction(&Instruction::End);
        self.code.function(&f.f);
        self.n_imports + self.funcs.len() - 1
    }

    /// How many functions this module imports — the offset a defined
    /// function's index starts from, which a lowering needs before any body
    /// exists in order to name a callee it has not emitted yet.
    pub fn n_imports(&self) -> u32 {
        self.n_imports
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
            self.exports.export("memory", ExportKind::Memory, 0);
        }

        let mut globals = GlobalSection::new();
        globals.global(
            GlobalType { val_type: ValType::I32, mutable: true, shared: false },
            &ConstExpr::i32_const(STACK_TOP as i32),
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

/// The module's `__stack_pointer`. Index 0 because it is the only global the
/// encoder defines — the emitter has no module-level mutable state that is not
/// in memory.
pub const SP: u32 = 0;

/// A function body plus the frame it was given.
pub struct Frame {
    f: Function,
    frame: u32,
    /// Local holding the frame's base address, valid for the whole body.
    base: u32,
}

impl Frame {
    /// Claim the frame. Subtracting past 0 wraps to near `0xFFFFFFFF`, where
    /// every access is out of bounds — which is the trap `--stack-first` buys,
    /// and the reason the stack is at the BOTTOM of memory rather than above the
    /// data it would otherwise overwrite.
    fn prologue(&mut self) {
        if self.frame == 0 {
            return;
        }
        self.f
            .instruction(&Instruction::GlobalGet(SP))
            .instruction(&Instruction::I32Const(self.frame as i32))
            .instruction(&Instruction::I32Sub)
            .instruction(&Instruction::LocalTee(self.base))
            .instruction(&Instruction::GlobalSet(SP));
    }

    /// Release it. Adding back to the base is the same value the prologue
    /// found, without a second local to hold it.
    fn epilogue(&mut self) {
        if self.frame == 0 {
            return;
        }
        self.f
            .instruction(&Instruction::LocalGet(self.base))
            .instruction(&Instruction::I32Const(self.frame as i32))
            .instruction(&Instruction::I32Add)
            .instruction(&Instruction::GlobalSet(SP));
    }

    /// Append one instruction.
    ///
    /// M1 note carried into M2: a body must not emit `return`, because it would
    /// jump past the epilogue and leak the frame. Returns go through a `br` to
    /// the function's outermost block, or the epilogue moves into a helper the
    /// return path calls.
    pub fn ins(&mut self, i: &Instruction) -> &mut Self {
        self.f.instruction(i);
        self
    }

    /// Push the address of the frame slot at `off`.
    pub fn slot(&mut self, off: u32) -> &mut Self {
        self.f.instruction(&Instruction::LocalGet(self.base));
        if off != 0 {
            self.f
                .instruction(&Instruction::I32Const(off as i32))
                .instruction(&Instruction::I32Add);
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

    /// An imported memory moves the memory section away and takes the export
    /// with it — the RFC-0076 split shape, where the shim owns the address space.
    #[test]
    fn importing_memory_drops_the_memory_section() {
        let mut m = Module::new();
        m.import_memory();
        let f = m.func(&[], &[], &[], 0, |_| {});
        m.export("vyrn_entry", f);
        assert_eq!(section_ids(&m.finish()), vec![1, 2, 3, 6, 7, 10]);
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

    /// Frames are rounded, and an empty one costs no instructions — but still
    /// takes its local, so a body's local numbering never depends on it.
    #[test]
    fn an_empty_frame_emits_no_prologue() {
        let mut m = Module::new();
        m.func(&[ValType::I32], &[], &[], 0, |b| {
            assert_eq!(b.base(), 1);
        });
        m.func(&[], &[], &[], 1, |b| {
            assert_eq!(b.frame, FRAME_ALIGN, "frames round up to the stack alignment");
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
