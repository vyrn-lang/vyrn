//! The wasm module encoder the direct backend emits bodies into. It
//! frames sections, interns data, hands out function indices, and wraps each
//! body in the shadow-stack prologue and epilogue.
//!
//! [`Module::finish`] alone writes sections, in the order the format fixes.
//! The memory map: a [`STACK_BYTES`] shadow stack growing down from
//! [`STACK_TOP`], then data from [`DATA_BASE`] up, ending below
//! [`STATICS_LIMIT`]. A frame push past address 0 wraps and the first access
//! traps, instead of walking into the data.

use std::collections::HashMap;
pub use wasm_encoder::{BlockType, Instruction, MemArg, ValType};
use wasm_encoder::{
    CodeSection, ConstExpr, CustomSection, DataSection, EntityType, ExportKind, ExportSection,
    Function, FunctionSection, GlobalSection, GlobalType, ImportSection, MemorySection, MemoryType,
    NameMap, NameSection, TypeSection,
};

/// Shadow stack bytes: [`vyrn_frontend::trap::CALL_DEPTH_LIMIT`] frames of
/// [`vyrn_frontend::trap::FRAME_LIMIT`] bytes, plus one page.
///
/// No accepted frame exceeds `FRAME_LIMIT`, so the call counter always trips
/// before the stack runs out, and a deep recursion stops with the same words
/// on every engine. The extra page holds runtime helpers' frames, which the
/// counter does not count. The total is 126 wasm pages of address space;
/// only the pages a recursion touches cost memory.
pub const STACK_BYTES: u32 =
    vyrn_frontend::trap::FRAME_LIMIT * vyrn_frontend::trap::CALL_DEPTH_LIMIT + 65_536;
/// Top of the generated module's shadow stack; it grows down from here to 0.
pub const STACK_TOP: u32 = STACK_BYTES;
/// First byte of the generated module's data segments; statics grow up from
/// here.
pub const DATA_BASE: u32 = STACK_BYTES;
/// The end of everything a module statically occupies must stay below this
/// address, about 8 MB above [`DATA_BASE`].
pub const STATICS_LIMIT: u32 = 16 * 1024 * 1024;

/// Bytes `std/runtime` occupies at `heapBase()` before its first block: the
/// free-list heads, the I/O buffers, the host imports' scratch, the digit
/// buffer. The map is in `std/runtime.vyrn` above `drain`. The declared memory
/// must cover it, or the first host call that uses the scratch writes past the
/// end of memory.
pub const HEAP_HEADER_BYTES: u32 = 8_768;

/// clang's frame alignment on wasm32.
const FRAME_ALIGN: u32 = 16;

/// An access at a static offset whose alignment hint is `2^align` bytes.
pub fn mem_arg(off: u32, align: u32) -> MemArg {
    MemArg {
        offset: off as u64,
        align,
        memory_index: 0,
    }
}

struct Imported {
    module: String,
    field: String,
    params: Vec<ValType>,
    results: Vec<ValType>,
}

/// A defined function; `body` is `None` while a reservation is outstanding.
struct Defined {
    params: Vec<ValType>,
    results: Vec<ValType>,
    body: Option<Frame>,
}

/// A module under construction.
///
/// Everything stays plain data until [`Module::finish`], because
/// [`Module::sweep`] renumbers calls, which encoded bytes cannot have.
pub struct Module {
    imports: Vec<Imported>,
    /// In index order. [`Module::reserve_func`] hands out an index whose body
    /// arrives later.
    bodies: Vec<Defined>,
    /// Function exports only; [`Module::finish`] adds the memory export and
    /// [`Module::export_entry_state`]'s.
    exports: Vec<(String, u32)>,
    sweep: bool,
    /// The single data segment, packed at [`DATA_BASE`]; `pool_at` deduplicates
    /// identical contents.
    pool: Vec<u8>,
    pool_at: HashMap<Vec<u8>, u32>,
    /// Every [`Module::data`] entry as `(address, length)`, in increasing
    /// address order. A [`Module::reserve`] is not here: its bytes are zeros.
    spans: Vec<(u32, u32)>,
    /// The end of the last [`Module::reserve`]; the sweep never cuts below it.
    reserved: u32,
    /// Emitted last, in the order added.
    custom: Vec<(String, Vec<u8>)>,
    /// Function names for a `name` section, by index as handed out. Empty
    /// unless the lowering asked for them (`VYRN_WASM_NAMES`).
    names: Vec<(u32, String)>,
    /// The nesting words' address, when [`Module::export_entry_state`] asked.
    nesting: Option<u32>,
}

impl Default for Module {
    fn default() -> Self {
        Self::new()
    }
}

impl Module {
    /// Returns an empty module that defines and exports its own memory.
    pub fn new() -> Self {
        Module {
            imports: Vec::new(),
            bodies: Vec::new(),
            exports: Vec::new(),
            sweep: false,
            pool: Vec::new(),
            pool_at: HashMap::new(),
            spans: Vec::new(),
            reserved: DATA_BASE,
            custom: Vec::new(),
            names: Vec::new(),
            nesting: None,
        }
    }

    /// Exports the stack pointer as [`SP_EXPORT`] and the address `nesting` of
    /// the region nesting and call-depth words as [`NESTING_EXPORT`]. A trap
    /// abandons the guest's frames and regions without giving them back, so a
    /// host that calls this module again after a trap restores all three to
    /// what it read before the call.
    pub fn export_entry_state(&mut self, nesting: u32) {
        self.nesting = Some(nesting);
    }

    /// Adds a custom section, emitted after every defined section.
    pub fn custom(&mut self, name: &str, payload: Vec<u8>) {
        self.custom.push((name.to_string(), payload));
    }

    /// Makes [`Module::finish`] keep only the functions, imports and data an
    /// export can reach over the emitted calls, and renumber. Call it after the
    /// last [`Module::fill`].
    pub fn sweep(&mut self) {
        self.sweep = true;
    }

    /// Declares an imported function and returns its index.
    ///
    /// # Panics
    ///
    /// If a function is already defined: imports take the bottom of the shared
    /// index space.
    pub fn import(
        &mut self,
        module: &str,
        field: &str,
        params: &[ValType],
        results: &[ValType],
    ) -> u32 {
        assert!(
            self.bodies.is_empty(),
            "import {module}.{field} declared after a defined function: \
             imports and definitions share one index space, imports first"
        );
        self.imports.push(Imported {
            module: module.to_string(),
            field: field.to_string(),
            params: params.to_vec(),
            results: results.to_vec(),
        });
        self.imports.len() as u32 - 1
    }

    /// Places `bytes` in the data segment at an `align`-aligned address and
    /// returns it. Identical contents at a compatible alignment share one.
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
        self.spans.push((at, bytes.len() as u32));
        at
    }

    /// Reserves `size` zero bytes at an `align`-aligned address and returns it.
    /// Unlike [`Module::data`], every reservation is its own address, because
    /// module state lives here.
    pub fn reserve(&mut self, size: u32, align: u32) -> u32 {
        debug_assert!(align.is_power_of_two());
        let at = DATA_BASE + round_up(self.pool.len() as u32, align);
        self.pool.resize((at - DATA_BASE + size) as usize, 0);
        self.reserved = at + size;
        at
    }

    /// Returns the first address past everything this module statically
    /// occupies.
    pub fn data_end(&self) -> u32 {
        DATA_BASE + self.pool.len() as u32
    }

    /// Defines a function built by `build` and returns its index.
    ///
    /// `frame` is the bytes reserved at the bottom of its shadow-stack frame;
    /// [`Frame::alloc`] grows the rest while the body is built, so the prologue
    /// is written after the body. Local indices are the parameters, then
    /// [`Frame::base`] (taken even for an empty frame), then `locals`, then
    /// each [`Frame::local`].
    pub fn func(
        &mut self,
        params: &[ValType],
        results: &[ValType],
        locals: &[ValType],
        frame: u32,
        build: impl FnOnce(&mut Frame),
    ) -> u32 {
        let mut f = Frame::new(params, results, locals, frame);
        build(&mut f);
        self.add(f)
    }

    /// Defines a function from a built body and returns its index. Use it
    /// instead of [`Module::func`] when the body needs the module while it is
    /// built.
    pub fn add(&mut self, f: Frame) -> u32 {
        self.bodies.push(Defined {
            params: f.params.clone(),
            results: f.results.clone(),
            body: Some(f),
        });
        self.next_func() - 1
    }

    /// Reserves a function index whose body [`Module::fill`] supplies later.
    /// A stored-closure dispatcher needs one: it is called mid-walk, but its body is
    /// complete only after the last body is walked.
    pub fn reserve_func(&mut self, params: &[ValType], results: &[ValType]) -> u32 {
        self.bodies.push(Defined {
            params: params.to_vec(),
            results: results.to_vec(),
            body: None,
        });
        self.next_func() - 1
    }

    /// Supplies the body of a function [`Module::reserve_func`] handed out.
    ///
    /// # Errors
    ///
    /// If `f` was built for another signature; the engine would refuse the
    /// module far from here (#444).
    ///
    /// # Panics
    ///
    /// If `index` was already filled.
    pub fn fill(&mut self, index: u32, f: Frame) -> Result<(), String> {
        let i = (index - self.n_imports()) as usize;
        let d = &mut self.bodies[i];
        if (&d.params, &d.results) != (&f.params, &f.results) {
            return Err(format!(
                "internal error: function {index} was reserved as {:?} -> {:?} and filled \
                 with a body built for {:?} -> {:?}",
                d.params, d.results, f.params, f.results
            ));
        }
        assert!(d.body.is_none(), "function {index} filled twice");
        d.body = Some(f);
        Ok(())
    }

    /// Returns the number of imports, the index the first defined function
    /// gets.
    pub fn n_imports(&self) -> u32 {
        self.imports.len() as u32
    }

    /// Returns the index the next defined function gets.
    pub fn next_func(&self) -> u32 {
        self.n_imports() + self.bodies.len() as u32
    }

    /// Exports function `func` under `name`, which makes it a sweep root.
    pub fn export(&mut self, name: &str, func: u32) -> &mut Self {
        self.exports.push((name.to_string(), func));
        self
    }

    /// Which functions an export reaches over the emitted calls, by index.
    fn reachable(&self) -> Vec<bool> {
        let n_imports = self.n_imports();
        let mut keep = vec![false; self.next_func() as usize];
        let mut work: Vec<u32> = self.exports.iter().map(|&(_, i)| i).collect();
        while let Some(i) = work.pop() {
            if std::mem::replace(&mut keep[i as usize], true) || i < n_imports {
                continue;
            }
            let d = &self.bodies[(i - n_imports) as usize];
            let body = d
                .body
                .as_ref()
                .unwrap_or_else(|| panic!("function {i} is reachable and was never filled"));
            for ins in &body.body {
                match ins {
                    Instruction::Call(c) => work.push(*c),
                    // The backend emits no function reference (a `fn` value is a
                    // tag and a direct call). One would hide an edge from the sweep.
                    Instruction::CallIndirect { .. }
                    | Instruction::ReturnCall(_)
                    | Instruction::ReturnCallIndirect { .. }
                    | Instruction::RefFunc(_) => {
                        panic!("sweep cannot see through {ins:?}: a function index that is not a `call`")
                    }
                    _ => {}
                }
            }
        }
        keep
    }

    /// Records the name of the function at `index` for the `name` section.
    pub fn name(&mut self, index: u32, name: &str) {
        self.names.push((index, name.to_string()));
    }

    /// Drops what no export reaches and renumbers the survivors in their old
    /// order. Calls, exports and names are the only places a function index
    /// appears; `reachable` panics on any other.
    fn prune(&mut self) {
        let keep = self.reachable();
        let n_imports = self.n_imports() as usize;
        let mut map = vec![u32::MAX; keep.len()];
        let mut next = 0u32;
        for (i, &k) in keep.iter().enumerate() {
            if k {
                map[i] = next;
                next += 1;
            }
        }
        let mut i = 0;
        self.imports.retain(|_| {
            i += 1;
            keep[i - 1]
        });
        let mut i = n_imports;
        self.bodies.retain(|_| {
            i += 1;
            keep[i - 1]
        });
        for d in &mut self.bodies {
            for ins in d.body.as_mut().expect("a reachable body").body.iter_mut() {
                if let Instruction::Call(c) = ins {
                    *c = map[*c as usize];
                    debug_assert_ne!(*c, u32::MAX, "a surviving call to a pruned function");
                }
            }
        }
        for (_, i) in &mut self.exports {
            *i = map[*i as usize];
        }
        self.names.retain_mut(|(i, _)| {
            *i = map[*i as usize];
            *i != u32::MAX
        });
        self.sweep_pool();
    }

    /// Zeroes every pool entry no surviving body reaches, then ends the pool at
    /// the last live entry or reservation. Addresses do not move, so a dead
    /// entry below that end keeps its address space and costs no module byte.
    ///
    /// An entry is live when a surviving body pushes an address inside it, or a
    /// live entry holds one: the trap table is addresses, and only its bytes
    /// name its rows. The test is containment, not equality, because `intern`
    /// hands out `at + SHDR` and a folded offset lands inside too. A constant
    /// that only looks like an address keeps an entry, which costs bytes and
    /// never behaviour.
    fn sweep_pool(&mut self) {
        let spans = &self.spans;
        let find = |x: u32| -> Option<usize> {
            let i = spans.partition_point(|&(at, _)| at <= x).checked_sub(1)?;
            let (at, len) = spans[i];
            (x - at < len).then_some(i)
        };
        let mut live = vec![false; spans.len()];
        let mut work: Vec<usize> = Vec::new();
        let mark = |i: usize, live: &mut Vec<bool>, work: &mut Vec<usize>| {
            if !live[i] {
                live[i] = true;
                work.push(i);
            }
        };
        for d in &self.bodies {
            for ins in &d.body.as_ref().expect("a surviving body").body {
                // A `memarg` offset is a field offset, never a whole address.
                let c = match ins {
                    Instruction::I32Const(c) => *c as u32,
                    Instruction::I64Const(c) => *c as u32,
                    _ => continue,
                };
                if let Some(i) = find(c) {
                    mark(i, &mut live, &mut work);
                }
            }
        }
        while let Some(i) = work.pop() {
            let (at, len) = self.spans[i];
            let lo = (at - DATA_BASE) as usize;
            let bytes = &self.pool[lo..lo + len as usize];
            // Unaligned: an entry's alignment says nothing about where a table
            // inside it puts its words.
            for w in bytes.windows(4) {
                let x = u32::from_le_bytes([w[0], w[1], w[2], w[3]]);
                if let Some(j) = find(x) {
                    mark(j, &mut live, &mut work);
                }
            }
        }
        let mut end = self.reserved;
        for (i, &(at, len)) in self.spans.iter().enumerate() {
            if live[i] {
                end = end.max(at + len);
            } else {
                let lo = (at - DATA_BASE) as usize;
                self.pool[lo..lo + len as usize].fill(0);
            }
        }
        self.pool.truncate((end - DATA_BASE) as usize);
    }

    /// Returns the module's bytes, sections in the order the format fixes. No
    /// other function emits a section.
    ///
    /// # Errors
    ///
    /// If the statics pass [`STATICS_LIMIT`], or two exports share a name
    /// (`memory` included).
    ///
    /// # Panics
    ///
    /// If a kept reservation was never filled.
    pub fn finish(mut self) -> Result<Vec<u8>, String> {
        if self.sweep {
            self.prune();
        }
        if self.data_end() > STATICS_LIMIT {
            let room = STATICS_LIMIT - DATA_BASE;
            return Err(format!(
                "this module's statics need {} bytes, {} of {room}\n  \
                 note: the data segments begin at {DATA_BASE}, where the shadow stack ends, and \
                 must end below {STATICS_LIMIT}, where the runtime shim's own stack begins\n  \
                 note: the size is the sum of every string literal, regex table and \
                 module-state binding in the program AND in everything it imports; a large \
                 catalogue belongs in a file the program reads at run time, not in the module",
                self.data_end() - DATA_BASE,
                crate::STATICS_LIMIT_NEEDLE,
            ));
        }

        // Types are interned after the sweep, so a pruned function's signature
        // costs nothing.
        let mut types = TypeSection::new();
        let mut type_ids: HashMap<(Vec<ValType>, Vec<ValType>), u32> = HashMap::new();
        let mut ty = |params: &[ValType], results: &[ValType]| -> u32 {
            let key = (params.to_vec(), results.to_vec());
            if let Some(&i) = type_ids.get(&key) {
                return i;
            }
            let i = types.len();
            types
                .ty()
                .function(params.iter().copied(), results.iter().copied());
            type_ids.insert(key, i);
            i
        };
        let mut imports = ImportSection::new();
        for i in &self.imports {
            let t = ty(&i.params, &i.results);
            imports.import(&i.module, &i.field, EntityType::Function(t));
        }
        let mut funcs = FunctionSection::new();
        for d in &self.bodies {
            funcs.function(ty(&d.params, &d.results));
        }
        drop(ty);

        // Export names are one namespace across kinds, and nothing downstream
        // validates the bytes, so a duplicate is refused here.
        let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
        let state = match self.nesting {
            Some(_) => &[SP_EXPORT, NESTING_EXPORT][..],
            None => &[],
        };
        for name in self
            .exports
            .iter()
            .map(|(n, _)| n.as_str())
            .chain(state.iter().copied())
        {
            if !seen.insert(name) {
                return Err(format!(
                    "duplicate export `{name}`\n  \
                     note: `_start`, `__vyrn_malloc`, `__vyrn_free`, `{SP_EXPORT}` and \
                     `{NESTING_EXPORT}` are taken by the runtime; rename the function"
                ));
            }
        }
        if !seen.insert("memory") {
            return Err(
                "`export extern fn memory` collides with this module's own memory export\n  \
                 note: export names are one namespace across kinds; rename the function"
                    .to_string(),
            );
        }

        let mut exports = ExportSection::new();
        for (name, i) in &self.exports {
            exports.export(name, ExportKind::Func, *i);
        }

        let mem = MemoryType {
            // Covers the statics and the runtime header above them; `malloc`
            // grows memory from there.
            minimum: (round_up(self.data_end() + HEAP_HEADER_BYTES, 65_536) / 65_536) as u64,
            maximum: None,
            memory64: false,
            shared: false,
            page_size_log2: None,
        };
        let mut memories = MemorySection::new();
        memories.memory(mem);
        // A WASI host reads every iovec out of this export.
        exports.export("memory", ExportKind::Memory, 0);

        let mut globals = GlobalSection::new();
        globals.global(
            GlobalType {
                val_type: ValType::I32,
                mutable: true,
                shared: false,
            },
            &ConstExpr::i32_const(STACK_TOP as i32),
        );
        // [`HEAP_BASE`]: 16-aligned so a `malloc` result is aligned for
        // anything `layout` can put in it.
        globals.global(
            GlobalType {
                val_type: ValType::I32,
                mutable: false,
                shared: false,
            },
            &ConstExpr::i32_const(round_up(self.data_end(), 16) as i32),
        );
        if let Some(at) = self.nesting {
            globals.global(
                GlobalType {
                    val_type: ValType::I32,
                    mutable: false,
                    shared: false,
                },
                &ConstExpr::i32_const(at as i32),
            );
            exports.export(SP_EXPORT, ExportKind::Global, SP);
            exports.export(NESTING_EXPORT, ExportKind::Global, NESTING);
        }

        // Runs of zeros are not written, because wasm memory arrives zeroed. A
        // segment costs about nine bytes, so only a wider gap splits one.
        const GAP: usize = 16;
        let mut data = DataSection::new();
        let mut i = 0;
        while i < self.pool.len() {
            if self.pool[i] == 0 {
                i += 1;
                continue;
            }
            let start = i;
            let mut end = i + 1;
            let mut j = end;
            while j < self.pool.len() {
                if self.pool[j] != 0 {
                    end = j + 1;
                } else if j - end >= GAP {
                    break;
                }
                j += 1;
            }
            let at = DATA_BASE + start as u32;
            data.active(
                0,
                &ConstExpr::i32_const(at as i32),
                self.pool[start..end].iter().copied(),
            );
            i = end;
        }

        let mut code = CodeSection::new();
        for (i, d) in self.bodies.into_iter().enumerate() {
            let body = d
                .body
                .unwrap_or_else(|| panic!("function {i} was reserved and never filled"));
            code.function(&encode(body));
        }

        let mut m = wasm_encoder::Module::new();
        m.section(&types);
        if !imports.is_empty() {
            m.section(&imports);
        }
        m.section(&funcs);
        m.section(&memories);
        m.section(&globals);
        m.section(&exports);
        m.section(&code);
        if !data.is_empty() {
            m.section(&data);
        }
        if !self.names.is_empty() {
            let mut fns = NameMap::new();
            self.names.sort_by_key(|(i, _)| *i);
            for (i, n) in &self.names {
                fns.append(*i, n);
            }
            let mut names = NameSection::new();
            names.functions(&fns);
            m.section(&names);
        }
        for (name, payload) in &self.custom {
            m.section(&CustomSection {
                name: name.as_str().into(),
                data: payload.as_slice().into(),
            });
        }
        Ok(m.finish())
    }
}

/// One finished body, with the shadow-stack prologue and epilogue around it.
fn encode(f: Frame) -> Function {
    let mut decl = vec![ValType::I32]; // the frame base
    decl.extend(f.locals.iter().copied());
    let mut out = Function::new_with_locals_types(decl);
    let frame = f.bytes();
    // Claim the frame. Subtracting past 0 wraps, and every access then traps
    // instead of overwriting the data above. Only a hand-built `Frame` reaches
    // the wrap (`tests/wasm_runs.rs`); [`STACK_BYTES`] covers accepted code.
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
    // Base plus frame is the value the prologue found.
    if frame != 0 {
        out.instruction(&Instruction::LocalGet(f.base))
            .instruction(&Instruction::I32Const(frame as i32))
            .instruction(&Instruction::I32Add)
            .instruction(&Instruction::GlobalSet(SP));
    }
    out.instruction(&Instruction::End);
    out
}

/// The global index of the module's `__stack_pointer`.
pub const SP: u32 = 0;

/// The export name of [`SP`]; see [`Module::export_entry_state`].
pub const SP_EXPORT: &str = "__stack_pointer";

/// The export name of an immutable global holding the address of two `i32`
/// words: the open regions, then the calls in flight.
pub const NESTING_EXPORT: &str = "__vyrn_nesting";

/// The global index of the first heap byte, 16-aligned past the statics.
/// Immutable, so `free` of an address below it, such as a `String` literal in
/// the data segment, is a no-op.
pub const HEAP_BASE: u32 = 1;

/// The global index of [`NESTING_EXPORT`], in a module that has it.
const NESTING: u32 = 2;

/// `memory.copy` within the one memory: pops the length, the source and the destination.
pub const MEMORY_COPY: Instruction<'static> = Instruction::MemoryCopy {
    src_mem: 0,
    dst_mem: 0,
};

/// A function body under construction and its shadow-stack frame. The
/// instructions are buffered because the prologue depends on the final frame
/// size.
pub struct Frame {
    body: Vec<Instruction<'static>>,
    locals: Vec<ValType>,
    next_local: u32,
    frame: u32,
    /// The most `frame` has ever been: what the prologue claims. `frame`
    /// itself comes back down at a statement's end ([`Frame::reset`]).
    high: u32,
    /// Slots given back below the top, as `(mark, end)`; see
    /// [`Frame::give_back`].
    freed: Vec<(u32, u32)>,
    /// Local holding the frame's base address, valid for the whole body.
    base: u32,
    params: Vec<ValType>,
    results: Vec<ValType>,
}

impl Frame {
    /// Returns an empty body with `locals` declared after the frame base and
    /// `frame` bytes reserved before anything [`Frame::alloc`] adds.
    pub fn new(params: &[ValType], results: &[ValType], locals: &[ValType], frame: u32) -> Self {
        let base = params.len() as u32;
        Frame {
            body: Vec::new(),
            locals: locals.to_vec(),
            next_local: base + 1 + locals.len() as u32,
            frame,
            high: frame,
            freed: Vec::new(),
            base,
            params: params.to_vec(),
            results: results.to_vec(),
        }
    }

    /// Appends one instruction. Never append `return`: it skips the epilogue
    /// and leaks the frame; branch to the outermost block instead.
    pub fn ins(&mut self, i: &Instruction<'static>) -> &mut Self {
        self.body.push(i.clone());
        self
    }

    /// Returns the instruction count, to hand [`Frame::rewind`] later.
    pub fn here(&self) -> usize {
        self.body.len()
    }

    /// Drops every instruction appended since `at`, a [`Frame::here`].
    pub fn rewind(&mut self, at: usize) {
        debug_assert!(at <= self.body.len());
        self.body.truncate(at);
    }

    /// Takes another local of type `t` and returns its index.
    pub fn local(&mut self, t: ValType) -> u32 {
        self.locals.push(t);
        self.next_local += 1;
        self.next_local - 1
    }

    /// Takes `size` bytes of frame at `align` and returns the offset from the
    /// frame base. A slot inside a loop is one slot, written afresh each turn.
    /// The total saturates, so an oversized frame fails the caller's
    /// `FRAME_LIMIT` check on [`Self::bytes`] instead of panicking.
    ///
    /// A slot belongs to a statement unless the statement bound a name: the
    /// walker takes [`Frame::mark`] before each statement and
    /// [`Frame::reset`]s after one that left the scope as it found it. A named
    /// slot goes back at the end of its extent ([`Frame::give_back`]).
    pub fn alloc(&mut self, size: u32, align: u32) -> u32 {
        debug_assert!(align.is_power_of_two());
        let at = round_up(self.frame, align.max(1));
        self.frame = at.saturating_add(size);
        self.high = self.high.max(self.frame);
        at
    }

    /// Returns where the next [`Frame::alloc`] lands, to hand [`Frame::reset`]
    /// later.
    pub fn mark(&self) -> u32 {
        self.frame
    }

    /// Gives back every slot taken since `mark`; the caller guarantees nothing
    /// still names one. A frame already below `mark` stays there.
    pub fn reset(&mut self, mark: u32) {
        if mark < self.frame {
            self.frame = mark;
            self.freed.retain(|&(_, end)| end <= mark);
        }
    }

    /// Gives back the slot from `from`, the [`Frame::mark`] before its
    /// [`Frame::alloc`], to `to`. A slot below the top waits until every slot
    /// above it is given back.
    pub fn give_back(&mut self, from: u32, to: u32) {
        self.freed.push((from, to));
        while let Some(i) = self.freed.iter().position(|&(_, end)| end == self.frame) {
            self.frame = self.freed.swap_remove(i).0;
        }
    }

    /// Pushes the address of the frame slot at `off`.
    pub fn slot(&mut self, off: u32) -> &mut Self {
        self.body.push(Instruction::LocalGet(self.base));
        if off != 0 {
            self.body.push(Instruction::I32Const(off as i32));
            self.body.push(Instruction::I32Add);
        }
        self
    }

    /// Copies `n` bytes from the address on top of the stack to the address under it.
    pub fn copy(&mut self, n: u32) -> &mut Self {
        self.body.push(Instruction::I32Const(n as i32));
        self.body.push(MEMORY_COPY);
        self
    }

    /// Returns the local holding the frame base.
    pub fn base(&self) -> u32 {
        self.base
    }

    /// Returns the bytes of shadow stack the prologue claims, rounded as
    /// [`encode`] rounds them.
    pub fn bytes(&self) -> u32 {
        round_up(self.high, FRAME_ALIGN)
    }
}

fn round_up(n: u32, align: u32) -> u32 {
    n.saturating_add(align - 1) & !(align - 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The section ids of `wasm`, in order. A hand parser, so no dependency can
    /// share the encoder's mistake.
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
        let ids = section_ids(&m.finish().unwrap());
        assert_eq!(ids, vec![1, 2, 3, 5, 6, 7, 10, 11]);
        assert!(
            ids.windows(2).all(|w| w[0] < w[1]),
            "sections out of order: {ids:?}"
        );
    }

    #[test]
    fn the_data_pool_packs_and_shares() {
        let mut m = Module::new();
        assert_eq!(m.data(b"hello\0", 1), DATA_BASE);
        assert_eq!(
            m.data(b"hello\0", 1),
            DATA_BASE,
            "identical contents are one string"
        );
        assert_eq!(m.data(b"bye\0", 1), DATA_BASE + 6);
        assert_eq!(m.data(&[0u8; 8], 8), DATA_BASE + 16);
        assert_eq!(m.data_end(), DATA_BASE + 24);
    }

    #[test]
    fn a_reservation_is_never_shared_with_another() {
        let mut m = Module::new();
        let a = m.reserve(8, 8);
        let b = m.reserve(8, 8);
        assert_ne!(a, b);
        assert_eq!(b, a + 8);
        assert_eq!(
            m.data(b"x\0", 1),
            b + 8,
            "a later string packs after the reservation"
        );
        assert_eq!(m.reserve(4, 4), b + 12);
    }

    #[test]
    fn the_sweep_cuts_a_dead_tail_from_the_statics() {
        let mut m = Module::new();
        let live = m.data(b"live", 1);
        let cell = m.reserve(4, 4);
        m.data(b"dead", 1);
        let f = m.func(&[], &[], &[], 0, |b| {
            b.ins(&Instruction::I32Const(live as i32))
                .ins(&Instruction::Drop);
        });
        m.export("_start", f);
        m.prune();
        assert_eq!(m.data_end(), cell + 4);
    }

    /// An empty frame emits no prologue but still takes its base local.
    #[test]
    fn an_empty_frame_emits_no_prologue() {
        let mut m = Module::new();
        let empty = m.func(&[ValType::I32], &[], &[], 0, |b| {
            assert_eq!(b.base(), 1);
        });
        let framed = m.func(&[], &[], &[], 1, |_| {});
        assert!(framed > empty);
    }

    #[test]
    fn slots_and_locals_are_taken_as_the_body_needs_them() {
        let mut m = Module::new();
        m.func(&[ValType::I64], &[], &[ValType::I32], 0, |b| {
            // params 0, the frame base 1, the pre-declared local 2, then ours.
            assert_eq!(b.local(ValType::I64), 3);
            assert_eq!(b.local(ValType::I32), 4);
            assert_eq!(b.alloc(4, 4), 0);
            assert_eq!(b.alloc(8, 8), 8);
            assert_eq!(b.alloc(1, 1), 16);
        });
    }

    #[test]
    fn a_reserved_index_is_callable_before_its_body_exists() {
        let mut m = Module::new();
        let later = m.reserve_func(&[], &[ValType::I32]);
        let caller = m.func(&[], &[ValType::I32], &[], 0, |b| {
            b.ins(&Instruction::Call(later));
        });
        assert_eq!(later, 0, "the reservation took the first index");
        assert_eq!(caller, 1);
        let mut f = Frame::new(&[], &[ValType::I32], &[], 0);
        f.ins(&Instruction::I32Const(7));
        m.fill(later, f).expect("the body matches its reservation");
        m.export("vyrn_entry", caller);
        assert_eq!(section_ids(&m.finish().unwrap()), vec![1, 3, 5, 6, 7, 10]);
    }

    /// (#444)
    #[test]
    fn a_body_for_another_signature_is_refused() {
        let mut m = Module::new();
        let later = m.reserve_func(&[ValType::I64, ValType::I64], &[ValType::I64]);
        let f = Frame::new(&[ValType::I32, ValType::I64], &[ValType::I64], &[], 0);
        let e = m.fill(later, f).expect_err("the signatures differ");
        assert!(
            e.contains("function 0 was reserved as [I64, I64] -> [I64]"),
            "{e}"
        );
    }

    /// An unreached unfilled reservation is not an error. `tests/wasm_runs.rs`
    /// checks that the surviving calls were renumbered.
    #[test]
    fn the_sweep_takes_the_imports_and_the_bodies_nothing_reaches() {
        let mut m = Module::new();
        let exit = m.import("wasi_snapshot_preview1", "proc_exit", &[ValType::I32], &[]);
        m.import(
            "wasi_snapshot_preview1",
            "fd_write",
            &[ValType::I32; 4],
            &[ValType::I32],
        );
        m.reserve_func(&[], &[ValType::F64]);
        let start = m.func(&[], &[], &[], 0, |b| {
            b.ins(&Instruction::I32Const(0))
                .ins(&Instruction::Call(exit));
        });
        m.export("_start", start);
        m.sweep();
        let bytes = m.finish().unwrap();
        assert!(
            bytes.windows(9).any(|w| w == b"proc_exit"),
            "the reached import stays"
        );
        assert!(
            !bytes.windows(8).any(|w| w == b"fd_write"),
            "the unreached import goes"
        );
        // 0x7c is `f64`, used only by the unreached signature.
        assert!(!bytes.contains(&0x7c), "a type only a pruned function used");
    }

    #[test]
    fn the_sweep_takes_the_data_nothing_reaches() {
        let mut m = Module::new();
        let exit = m.import("wasi_snapshot_preview1", "proc_exit", &[ValType::I32], &[]);
        let named = m.data(b"kept-by-a-body\0", 4);
        let by_table = m.data(b"kept-by-a-table\0", 4);
        m.data(b"reached-by-nothing\0", 4);
        let table = m.data(&by_table.to_le_bytes(), 4);
        let start = m.func(&[], &[], &[], 0, |b| {
            b.ins(&Instruction::I32Const(named as i32))
                .ins(&Instruction::Drop)
                .ins(&Instruction::I32Const(table as i32))
                .ins(&Instruction::Drop)
                .ins(&Instruction::I32Const(0))
                .ins(&Instruction::Call(exit));
        });
        m.export("_start", start);
        m.sweep();
        let bytes = m.finish().unwrap();
        let has = |s: &[u8]| bytes.windows(s.len()).any(|w| w == s);
        assert!(has(b"kept-by-a-body"), "a literal the body names stays");
        assert!(
            has(b"kept-by-a-table"),
            "a literal a live table names stays"
        );
        assert!(!has(b"reached-by-nothing"), "the literal nobody names goes");
    }

    #[test]
    #[should_panic(expected = "is reachable and was never filled")]
    fn a_reachable_body_that_was_never_filled_is_caught_by_the_sweep() {
        let mut m = Module::new();
        let later = m.reserve_func(&[], &[]);
        let start = m.func(&[], &[], &[], 0, |b| {
            b.ins(&Instruction::Call(later));
        });
        m.export("_start", start);
        m.sweep();
        let _ = m.finish();
    }

    #[test]
    #[should_panic(expected = "reserved and never filled")]
    fn a_reservation_nobody_filled_is_not_a_module() {
        let mut m = Module::new();
        m.reserve_func(&[], &[]);
        let _ = m.finish();
    }

    #[test]
    #[should_panic(expected = "imports and definitions share one index space")]
    fn an_import_after_a_definition_is_an_index_bug_waiting_to_happen() {
        let mut m = Module::new();
        m.func(&[], &[], &[], 0, |_| {});
        m.import("env", "late", &[], &[]);
    }

    /// [`HEAP_HEADER_BYTES`] copies a Vyrn number; the map above `drain` in
    /// `std/runtime.vyrn` is the source.
    #[test]
    fn the_runtime_header_is_the_size_std_runtime_says_it_is() {
        let map = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../std/runtime.vyrn")
            .canonicalize()
            .expect("std/runtime.vyrn is in the tree");
        let src = std::fs::read_to_string(&map).expect("read std/runtime.vyrn");
        let line = src
            .lines()
            .find(|l| l.contains("the allocator's first block"))
            .expect("the heap map names the allocator's first block");
        let at: u32 = line
            .split_whitespace()
            .nth(1)
            .and_then(|w| w.parse().ok())
            .expect("the row starts with the offset");
        assert_eq!(
            at, HEAP_HEADER_BYTES,
            "std/runtime.vyrn puts its first block at {at}, and the emitted memory \
             reserves {HEAP_HEADER_BYTES} for the header in front of it"
        );
    }

    /// The boundary case: data ending exactly on a page needs another page.
    #[test]
    fn the_declared_memory_covers_the_runtime_header() {
        let mut m = Module::new();
        m.data(&vec![7u8; (65_536 - DATA_BASE % 65_536) as usize], 1);
        let f = m.func(&[], &[], &[], 0, |_| {});
        m.export("_start", f);
        let end = m.data_end();
        assert_eq!(end % 65_536, 0, "the witness is data ending on a page");
        let bytes = m.finish().expect("finish");
        let pages = read_memory_minimum(&bytes);
        assert!(
            pages * 65_536 >= end + HEAP_HEADER_BYTES,
            "{pages} pages stop at {} and the header ends at {}",
            pages * 65_536,
            end + HEAP_HEADER_BYTES
        );
    }

    fn read_memory_minimum(bytes: &[u8]) -> u32 {
        let mut i = 8;
        while i < bytes.len() {
            let id = bytes[i];
            i += 1;
            let (size, next) = uleb(bytes, i);
            i = next;
            if id == 5 {
                let (_count, j) = uleb(bytes, i);
                let (_flags, j) = (bytes[j], j + 1);
                return uleb(bytes, j).0;
            }
            i += size as usize;
        }
        panic!("no memory section");
    }

    fn uleb(bytes: &[u8], mut i: usize) -> (u32, usize) {
        let (mut out, mut shift) = (0u32, 0);
        loop {
            let b = bytes[i];
            i += 1;
            out |= ((b & 0x7f) as u32) << shift;
            shift += 7;
            if b & 0x80 == 0 {
                return (out, i);
            }
        }
    }

    #[test]
    fn statics_past_the_line_the_shim_needs_are_a_diagnostic() {
        let mut m = Module::new();
        m.data(&vec![0u8; STATICS_LIMIT as usize], 1);
        let e = m
            .finish()
            .expect_err("statics past the limit must be refused");
        assert!(
            e.contains(crate::STATICS_LIMIT_NEEDLE),
            "the refusal must carry the needle a test can pin: {e}"
        );
        assert!(
            e.contains(&(STATICS_LIMIT - DATA_BASE).to_string()),
            "the refusal must name the room a module actually has: {e}"
        );
    }

    /// The control for the test above.
    #[test]
    fn statics_that_just_fit_still_finish() {
        let mut m = Module::new();
        m.data(&vec![0u8; (STATICS_LIMIT - DATA_BASE) as usize], 1);
        let f = m.func(&[], &[], &[], 0, |_| {});
        m.export("_start", f);
        assert!(m.finish().is_ok());
    }

    #[test]
    fn a_user_export_named_memory_is_refused_not_emitted() {
        let mut m = Module::new();
        let f = m.func(&[], &[], &[], 0, |_| {});
        m.export("memory", f);
        let e = m
            .finish()
            .expect_err("a memory export collision must be refused");
        assert!(e.contains("memory"), "the refusal names the export: {e}");
    }

    #[test]
    fn two_exports_of_one_name_are_refused() {
        let mut m = Module::new();
        let a = m.func(&[], &[], &[], 0, |_| {});
        let b = m.func(&[], &[], &[], 0, |_| {});
        m.export("_start", a);
        m.export("_start", b);
        assert!(m.finish().is_err());
    }
}
