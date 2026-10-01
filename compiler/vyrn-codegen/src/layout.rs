//! Size, alignment and field offsets of the shapes `llt_of` prints, which give
//! the wasm emitter its literal load and store offsets. It parses `llt_of`'s
//! string instead of matching on `Type`, so layout cannot drift from lowering.
//! The target is wasm32 (`p:32:32`, `i64:64`): a pointer is 4 bytes and an `i64`
//! is 8-aligned, so `{ ptr, i64, i64 }` is 24 bytes with a hole, not 20. A struct
//! places each member at the next multiple of its alignment and pads its size to
//! its widest member's alignment, as LLVM and clang do; a vector's alignment is
//! its size rounded up to a power of two. Every size is a checked `u32`: a
//! wrapped size would pass every later bound, so [`fits`] refuses it.

use crate::wasm::{mem_arg, Instruction, ValType};

/// One machine scalar on wasm32: what a load, a store or a call boundary moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Leaf {
    /// A `Bool`: one byte in memory, holding 0 or 1.
    I1,
    I8,
    I16,
    I32,
    I64,
    /// A 4-byte address.
    Ptr,
    F32,
    F64,
    /// Every SIMD type. The instruction decides the lane interpretation, and a
    /// mask is all-ones or all-zeros lanes.
    V128,
}

impl Leaf {
    /// Bytes in memory.
    pub fn size(self) -> u32 {
        match self {
            Leaf::I1 | Leaf::I8 => 1,
            Leaf::I16 => 2,
            Leaf::I32 | Leaf::Ptr | Leaf::F32 => 4,
            Leaf::I64 | Leaf::F64 => 8,
            Leaf::V128 => 16,
        }
    }

    /// Every leaf is aligned to its size, as clang lays it out on wasm32.
    pub fn align(self) -> u32 {
        self.size()
    }

    /// The wasm value type a leaf travels as: wasm has no value narrower than
    /// `i32`, so `I1`, `I8`, `I16` and `Ptr` widen to it.
    pub fn val_type(self) -> ValType {
        match self {
            Leaf::I1 | Leaf::I8 | Leaf::I16 | Leaf::I32 | Leaf::Ptr => ValType::I32,
            Leaf::I64 => ValType::I64,
            Leaf::F32 => ValType::F32,
            Leaf::F64 => ValType::F64,
            Leaf::V128 => ValType::V128,
        }
    }

    /// The load of this leaf at a static offset, at its natural alignment.
    ///
    /// `I8` is both `Int8` and `UInt8`, so `signed` carries the sign across the
    /// load. The other leaves ignore it: their carrier is their width, and a
    /// `Bool` is a byte of 0 or 1.
    pub fn load(self, off: u32, signed: bool) -> Instruction<'static> {
        let m = |align| mem_arg(off, align);
        match self {
            Leaf::I64 => Instruction::I64Load(m(3)),
            Leaf::F64 => Instruction::F64Load(m(3)),
            Leaf::F32 => Instruction::F32Load(m(2)),
            Leaf::I32 | Leaf::Ptr => Instruction::I32Load(m(2)),
            Leaf::I16 if signed => Instruction::I32Load16S(m(1)),
            Leaf::I16 => Instruction::I32Load16U(m(1)),
            Leaf::I8 if signed => Instruction::I32Load8S(m(0)),
            Leaf::I8 | Leaf::I1 => Instruction::I32Load8U(m(0)),
            // `align: 0` understates on purpose, as `@f32x4Load` does: a frame is
            // only 8-aligned, and an overstated hint is a lie the engine may act on.
            Leaf::V128 => Instruction::V128Load(m(0)),
        }
    }

    /// The store of this leaf at offset 0. See [`Leaf::load`] for the `V128` hint.
    pub fn store(self) -> Instruction<'static> {
        let m = |align| mem_arg(0, align);
        match self {
            Leaf::I64 => Instruction::I64Store(m(3)),
            Leaf::F64 => Instruction::F64Store(m(3)),
            Leaf::F32 => Instruction::F32Store(m(2)),
            Leaf::I32 | Leaf::Ptr => Instruction::I32Store(m(2)),
            Leaf::I16 => Instruction::I32Store16(m(1)),
            Leaf::I8 | Leaf::I1 => Instruction::I32Store8(m(0)),
            Leaf::V128 => Instruction::V128Store(m(0)),
        }
    }
}

/// The machine shape of a type: what its bytes are, before any offset is
/// computed. Two types with equal shapes have one representation.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Shape {
    /// No bytes: `Unit`, `Never`, and a type that does not resolve.
    Void,
    Leaf(Leaf),
    /// Members in order, each at the next multiple of its alignment.
    Struct(Vec<Shape>),
    /// `n` elements inline, with no header.
    Array(usize, Box<Shape>),
}

/// Where one shape's bytes are: its size, its alignment, and the offset of each
/// field (empty for scalars; for `[N x T]` the stride is `size / N`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    pub size: u32,
    pub align: u32,
    pub fields: Vec<u32>,
}

/// Named shapes that `llt_of` prints, each compared against clang's layout.
///
/// Most rows are padding probes, chosen where clang and this engine could
/// disagree (`RecordNested`, `SmallArray_i8`, `RecordOfVector`). The rest are one
/// row per leaf spelling `llt_of` prints; the test
/// `llt_prints_every_shape_the_layout_engine_was_verified_on` holds that part
/// complete. A shape that embeds its element type appears at several widths.
pub const SHAPES: &[(&str, &str)] = &[
    ("Int64", "i64"),
    ("Int32", "i32"),
    ("Int16", "i16"),
    ("Int8", "i8"),
    ("Bool", "i1"),
    ("Float64", "double"),
    ("Float32", "float"),
    ("String", "ptr"),
    // `Array` leads with a 4-byte member and then needs `i64` alignment.
    ("Array", "{ ptr, i64, i64 }"),
    // The array triple, then the producer's tag, payload and cursor generation.
    ("Stream", "{ ptr, i64, i64, i64, i64, i64 }"),
    ("Map", "{ ptr, ptr, i64, i64, ptr }"),
    ("Ref", "{ i64, i64 }"),
    ("Fn", "{ i64, i64 }"),
    // One machine shape (16 bytes, 16-aligned) under four spellings; clang is
    // asked about each spelling `llt_of` prints.
    ("F32x4", "<4 x float>"),
    ("I32x4/Mask32x4", "<4 x i32>"),
    ("F64x2", "<2 x double>"),
    ("Mask64x2", "<2 x i64>"),
    // A vector inside a record, which is the padding case the bare vector cannot
    // show: 16-alignment pushes the member to 16 and the struct to 32.
    ("RecordOfVector", "{ i8, <4 x float> }"),
    // Sums: an `i64` tag plus one `i64` per payload slot of the widest variant.
    // `Option<Int64>` is `Enum1`; `Option<fn(Int64)>` is `Enum2`.
    ("Enum0", "{ i64 }"),
    ("Enum1", "{ i64, i64 }"),
    ("Enum2", "{ i64, i64, i64 }"),
    ("Enum3", "{ i64, i64, i64, i64 }"),
    ("RecordEmpty", "{  }"),
    ("RecordMixed", "{ i1, ptr, i64, i8, double }"),
    ("RecordNarrow", "{ i8, i8, i16 }"),
    ("RecordNested", "{ i8, { i8, { i8, i64 } }, i32 }"),
    ("RecordOfArray", "{ i8, { ptr, i64, i64 } }"),
    // In the i8 cases the inline buffer does not end on the struct's alignment.
    ("ArrayN_i64", "[4 x i64]"),
    ("ArrayN_i8", "[3 x i8]"),
    ("ArrayN_struct", "[2 x { i8, i64 }]"),
    ("SmallArray_i64", "{ i64, i64, ptr, [4 x i64] }"),
    ("SmallArray_i8", "{ i64, i64, ptr, [3 x i8] }"),
    ("SmallArray_str", "{ i64, i64, ptr, [2 x ptr] }"),
];

/// Returns the layout of one type string as `llt_of` prints it.
///
/// Errors rather than panics outside that grammar: the input is generated, so a
/// rejection is this crate contradicting itself, and the caller names the shape.
pub fn of_ll(ll: &str) -> Result<Layout, String> {
    let mut p = P {
        s: ll.as_bytes(),
        i: 0,
    };
    let l = p.ty()?;
    p.ws();
    if p.i != p.s.len() {
        return Err(format!("trailing text in type {ll:?} at byte {}", p.i));
    }
    Ok(l)
}

struct P<'a> {
    s: &'a [u8],
    i: usize,
}

impl P<'_> {
    fn ws(&mut self) {
        while self.i < self.s.len() && self.s[self.i].is_ascii_whitespace() {
            self.i += 1;
        }
    }

    fn eat(&mut self, c: u8) -> bool {
        self.ws();
        if self.i < self.s.len() && self.s[self.i] == c {
            self.i += 1;
            return true;
        }
        false
    }

    fn ty(&mut self) -> Result<Layout, String> {
        self.ws();
        match self.s.get(self.i) {
            Some(b'{') => self.strukt(),
            Some(b'[') => self.array(),
            Some(b'<') => self.vector(),
            Some(_) => self.scalar(),
            None => Err("unexpected end of type".to_string()),
        }
    }

    fn strukt(&mut self) -> Result<Layout, String> {
        self.i += 1;
        // In 64 bits: a total past 4 GB must be refused, and a wrapped `u32`
        // total would be accepted.
        let (mut size, mut align, mut fields) = (0u64, 1u32, Vec::<u64>::new());
        if !self.eat(b'}') {
            loop {
                let f = self.ty()?;
                size = round_up(size, f.align as u64);
                fields.push(size);
                size += f.size as u64;
                align = align.max(f.align);
                if !self.eat(b',') {
                    break;
                }
            }
            if !self.eat(b'}') {
                return Err(format!("expected `}}` at byte {}", self.i));
            }
        }
        // Tail padding, so `[N x S]` keeps every element aligned.
        let size = fits(round_up(size, align as u64), "a record")?;
        Ok(Layout {
            size,
            align,
            // Every offset is below the size that just fit, so none can overflow.
            fields: fields.iter().map(|f| *f as u32).collect(),
        })
    }

    fn array(&mut self) -> Result<Layout, String> {
        self.i += 1;
        let n = self.count()?;
        if !self.s[self.i..].starts_with(b"x") {
            return Err(format!("expected `x` at byte {}", self.i));
        }
        self.i += 1;
        let elem = self.ty()?;
        if !self.eat(b']') {
            return Err(format!("expected `]` at byte {}", self.i));
        }
        // Every size here is already rounded to its alignment, so it is the stride.
        Ok(Layout {
            size: fits(n as u64 * elem.size as u64, "a fixed array")?,
            align: elem.align,
            fields: Vec::new(),
        })
    }

    /// `<N x T>`: the size is the lanes, unpadded, and the alignment is that size
    /// rounded up to a power of two, as LLVM and clang lay out a vector.
    fn vector(&mut self) -> Result<Layout, String> {
        self.i += 1;
        let n = self.count()?;
        if !self.s[self.i..].starts_with(b"x") {
            return Err(format!("expected `x` at byte {}", self.i));
        }
        self.i += 1;
        let elem = self.ty()?;
        if !self.eat(b'>') {
            return Err(format!("expected `>` at byte {}", self.i));
        }
        let size = fits(n as u64 * elem.size as u64, "a vector")?;
        Ok(Layout {
            size,
            align: size.max(1).next_power_of_two(),
            fields: Vec::new(),
        })
    }

    /// Reads the `N` of `[N x T]` or `<N x T>` and the whitespace after it.
    fn count(&mut self) -> Result<u32, String> {
        self.ws();
        let start = self.i;
        while self.s.get(self.i).is_some_and(u8::is_ascii_digit) {
            self.i += 1;
        }
        let n = std::str::from_utf8(&self.s[start..self.i])
            .ok()
            .and_then(|d| d.parse().ok())
            .ok_or_else(|| format!("expected an element count at byte {start}"))?;
        self.ws();
        Ok(n)
    }

    fn scalar(&mut self) -> Result<Layout, String> {
        let start = self.i;
        while self
            .s
            .get(self.i)
            .is_some_and(|c| c.is_ascii_alphanumeric())
        {
            self.i += 1;
        }
        let word = std::str::from_utf8(&self.s[start..self.i]).unwrap_or("");
        let (size, align) = match word {
            "ptr" => (4, 4),
            "double" => (8, 8),
            "float" => (4, 4),
            // `void` appears only as a return type, so it never adds padding.
            "void" => (0, 1),
            // `i1` occupies a whole byte in memory (LLVM's alloc size).
            "i1" => (1, 1),
            "i8" => (1, 1),
            "i16" => (2, 2),
            "i32" => (4, 4),
            "i64" => (8, 8),
            _ => return Err(format!("unknown scalar type {word:?} at byte {start}")),
        };
        Ok(Layout {
            size,
            align,
            fields: Vec::new(),
        })
    }
}

/// Returns `bytes` as a `u32` size, or the refusal.
///
/// A wasm32 memory is `u32`-wide. A wrapped size would pass every later bound
/// (the frame limit, `malloc`, `memory.copy`), so the check sits where the size
/// is measured.
fn fits(bytes: u64, what: &str) -> Result<u32, String> {
    u32::try_from(bytes).map_err(|_| {
        format!(
            "{what} needs {bytes} bytes, past the {} one shape may occupy; \
             a fixed array this big belongs on the heap as `Array<T>`",
            u32::MAX
        )
    })
}

/// `n` rounded up to a multiple of `align`, in the callers' 64-bit width so the
/// rounding cannot overflow.
fn round_up(n: u64, align: u64) -> u64 {
    debug_assert!(align.is_power_of_two());
    (n + align - 1) & !(align - 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Written out rather than derived, so a parser change has to disagree with
    /// numbers a person wrote down.
    #[test]
    fn the_four_shapes_the_runtime_is_built_on() {
        // `Array<T>`, also `__vyrn_args()`'s return type: a 4-byte hole after
        // the pointer.
        let a = of_ll("{ ptr, i64, i64 }").unwrap();
        assert_eq!((a.size, a.align, &a.fields[..]), (24, 8, &[0, 8, 16][..]));
        // `Map<String, V>`: the trailing index `ptr` pads the size to 32, not 28.
        let m = of_ll("{ ptr, ptr, i64, i64, ptr }").unwrap();
        assert_eq!(
            (m.size, m.align, &m.fields[..]),
            (32, 8, &[0, 4, 8, 16, 24][..])
        );
        // A sum with two payload slots, such as `Option<fn(Int64)>`.
        let o = of_ll("{ i64, i64, i64 }").unwrap();
        assert_eq!((o.size, o.align, &o.fields[..]), (24, 8, &[0, 8, 16][..]));
        // A `fn` value: tag plus capture block.
        let r = of_ll("{ i64, i64 }").unwrap();
        assert_eq!((r.size, r.align, &r.fields[..]), (16, 8, &[0, 8][..]));
    }

    /// A `SmallArray<UInt8, 3>` whose buffer ends at 23 is 24 bytes, so an array
    /// of them stays aligned.
    #[test]
    fn tail_padding_rounds_a_struct_up_to_its_own_alignment() {
        let s = of_ll("{ i64, i64, ptr, [3 x i8] }").unwrap();
        assert_eq!(
            (s.size, s.align, &s.fields[..]),
            (24, 8, &[0, 8, 16, 20][..])
        );
        let w = of_ll("{ i64, i64, ptr, [4 x i64] }").unwrap();
        assert_eq!(
            (w.size, w.align, &w.fields[..]),
            (56, 8, &[0, 8, 16, 24][..])
        );
    }

    #[test]
    fn every_shape_the_emitter_can_print_has_a_layout() {
        for (name, ll) in SHAPES {
            let l = of_ll(ll).unwrap_or_else(|e| panic!("{name} ({ll}): {e}"));
            assert!(l.align.is_power_of_two(), "{name}: align {}", l.align);
            assert_eq!(
                l.size % l.align,
                0,
                "{name}: size {} not a multiple of align",
                l.size
            );
        }
    }

    #[test]
    fn malformed_shapes_are_reported_not_guessed() {
        assert!(of_ll("{ i64").is_err());
        assert!(of_ll("i64 i64").is_err());
        assert!(of_ll("i128").is_err());
        assert!(of_ll("[x i8]").is_err());
        assert!(of_ll("<4 x float").is_err());
        assert!(of_ll("<4 x >").is_err());
    }

    /// In a record, a vector's 16-alignment moves it to offset 16 and the size
    /// to 32.
    #[test]
    fn a_vector_is_sixteen_bytes_sixteen_aligned() {
        for ll in ["<4 x float>", "<4 x i32>", "<2 x double>", "<2 x i64>"] {
            let v = of_ll(ll).unwrap_or_else(|e| panic!("{ll}: {e}"));
            assert_eq!((v.size, v.align), (16, 16), "{ll}");
        }
        let r = of_ll("{ i8, <4 x float> }").unwrap();
        assert_eq!((r.size, r.align, &r.fields[..]), (32, 16, &[0, 16][..]));
        assert_eq!(of_ll("[3 x <2 x i64>]").unwrap().size, 48);
    }

    /// `536870912 * 8` is 2^32, which wraps to zero: a 4 GiB array that would
    /// pass the frame limit as needing no bytes.
    #[test]
    fn a_shape_past_four_gigabytes_is_refused_rather_than_wrapped() {
        for (ll, wrapped) in [
            ("[600000000 x i64]", 505_032_704u64),
            ("[536870912 x i64]", 0),
            ("[100000 x [100000 x i64]]", 2_690_588_672),
            ("{ i8, [600000000 x i64] }", 505_032_712),
        ] {
            let e = of_ll(ll).expect_err(&format!("{ll} wrapped to {wrapped} instead"));
            assert!(
                e.contains("bytes, past the 4294967295 one shape may occupy")
                    && e.contains("belongs on the heap as `Array<T>`"),
                "{ll}: {e}"
            );
        }
        // The largest shape that fits keeps its exact size.
        assert_eq!(of_ll("[536870911 x i64]").unwrap().size, 4_294_967_288);
    }
}
