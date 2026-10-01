//! The machine shape of a type ([`Shape`], built by `crate::shape_of`) and its
//! layout: size, alignment and field offsets, which give the wasm emitter its
//! literal load and store offsets. The target is wasm32: a pointer is 4 bytes
//! and an `i64` is 8-aligned, so `Array<T>`'s `{ ptr, i64, i64 }` is 24 bytes
//! with a hole, not 20. A struct places each member at the next multiple of its
//! alignment and pads its size to its widest member's alignment, as clang does.
//! Every size is a checked `u32`: a wrapped size would pass every later bound,
//! so [`fits`] refuses it.

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

impl Shape {
    /// Returns the layout of this shape, or the refusal of one past 4 GB.
    pub fn layout(&self) -> Result<Layout, String> {
        let scalar = |size, align| Layout {
            size,
            align,
            fields: Vec::new(),
        };
        Ok(match self {
            // No bytes, so no padding either.
            Shape::Void => scalar(0, 1),
            Shape::Leaf(l) => scalar(l.size(), l.align()),
            Shape::Struct(members) => {
                // In 64 bits: a total past 4 GB must be refused, and a wrapped
                // `u32` total would be accepted.
                let (mut size, mut align, mut fields) = (0u64, 1u32, Vec::new());
                for m in members {
                    let f = m.layout()?;
                    size = round_up(size, f.align as u64);
                    fields.push(size);
                    size += f.size as u64;
                    align = align.max(f.align);
                }
                // Tail padding, so `[N x S]` keeps every element aligned.
                let size = fits(round_up(size, align as u64).into(), "a record")?;
                Layout {
                    size,
                    align,
                    // Every offset is below the size that just fit, so none can
                    // overflow.
                    fields: fields.iter().map(|f| *f as u32).collect(),
                }
            }
            // Every size is already rounded to its alignment, so it is the
            // stride.
            Shape::Array(n, elem) => {
                let e = elem.layout()?;
                scalar(fits(*n as u128 * e.size as u128, "a fixed array")?, e.align)
            }
        })
    }
}

/// Returns `bytes` as a `u32` size, or the refusal.
///
/// A wasm32 memory is `u32`-wide. A wrapped size would pass every later bound
/// (the frame limit, `malloc`, `memory.copy`), so the check sits where the size
/// is measured.
fn fits(bytes: u128, what: &str) -> Result<u32, String> {
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
