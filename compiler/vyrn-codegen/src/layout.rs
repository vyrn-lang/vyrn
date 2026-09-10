//! Size, alignment and field offsets for the shapes this crate emits
//! (RFC-0077 M0).
//!
//! Today nobody computes these: the textual backend writes `getelementptr` and
//! lets LLVM do the arithmetic, and `sizeof` is the null-GEP trick. A direct
//! wasm emitter has no such luxury — every `i32.load` needs a literal offset —
//! so the numbers have to exist somewhere, and the only interesting question is
//! whether they agree with the ones the current backend produces. A disagreement
//! is a silent miscompile, not a link error.
//!
//! Which is why this takes the LLVM type STRING rather than a [`Type`]. There is
//! exactly one function in this crate that maps a Vyrn type to a shape — `llt`,
//! plus its two helpers `enum_ll` and `sa_ll` — and consuming its output means
//! layout cannot drift from lowering: adding a case to `llt` changes what this
//! sees, automatically. Going the other way (a second match on `Type`) would be
//! two sources of truth for one fact, which is the mistake this RFC exists to
//! avoid making twice. So the type → layout path is `llt` ∘ [`of_ll`].
//!
//! Two smaller things fall out of the same choice. A string is finite, so a
//! record type that somehow referred to itself could not hang the engine (it
//! could not have been printed in the first place). And `llt`'s output is what
//! the emitter will read for allocas and GEPs anyway, so nothing is parsed that
//! was not going to be produced.
//!
//! # The rules
//!
//! wasm32-wasip1's data layout, the only target this module is for:
//! `e-m:e-p:32:32-p10:8:8-p20:8:8-i64:64-n32:64-S128`. What matters out of that
//! is `p:32:32` (pointers are 4 bytes, 4-aligned) and `i64:64` — an `i64` is
//! 8-ALIGNED on wasm32, unlike i386 where the same struct would pack tighter.
//! That single difference is what `{ ptr, i64, i64 }` (the growable-array
//! triple) turns on: 24 bytes with a 4-byte hole, not 20. It is verified against
//! clang rather than asserted, in `tests/layout_vs_clang.rs`.
//!
//! Struct layout is the ordinary sequential rule — each member starts at the
//! next multiple of its alignment, the struct's alignment is its widest member's,
//! and the size is rounded up to that so an array of the struct stays aligned.
//! LLVM lays literal structs out this way and clang lays plain C structs out this
//! way, which is what makes the C comparison meaningful.
//!
//! A vector (`<4 x float>` and its three siblings, RFC-0083) is the one shape
//! that is not a struct rule: its size is its lanes, and its alignment is that
//! size rounded to a power of two, which is what LLVM and clang both do.
//!
//! # The one limit
//!
//! Every number here is a `u32`, which is also the whole of a wasm32 address
//! space, so the arithmetic is CHECKED against it rather than left to wrap. That
//! is not tidiness: `Array<Int64, 600000000>` is 4.8 GB, and a wrapped product
//! is a SMALL number describing a huge shape. Nothing downstream can catch it —
//! the frame bound compares the wrapped size, `malloc` takes the wrapped byte
//! count, and `memory.copy` reads the same number as unsigned — so the shape has
//! to be refused where it is measured. [`fits`] is where.

/// Where one shape's bytes are: its size, its alignment, and the offset of each
/// field (empty for scalars; for `[N x T]` the stride is `size / N`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    pub size: u32,
    pub align: u32,
    pub fields: Vec<u32>,
}

/// What clang is asked about: a name and the shape `llt` prints for it.
///
/// This is a chosen list, not a total one, and the comment that used to call it
/// "the emitter's whole type universe" was wrong in a way that cost something —
/// `Stream` and `Ref` sat here with no case in the test that was supposed to
/// keep them honest, and RFC-0083's four vector spellings were printed by `llt`,
/// refused by [`of_ll`], and never once compared against clang.
///
/// Two different jobs are mixed here on purpose. Most rows are PADDING probes,
/// chosen because clang and this engine could plausibly disagree about them:
/// `RecordNested`, `SmallArray_i8`, `RecordOfVector`. The rest are one row per
/// LEAF spelling `llt` can print, which is the part that has to be complete.
///
/// `llt_prints_every_shape_the_layout_engine_was_verified_on` in this crate's
/// tests is what makes it complete: it builds a `Type` for every variant of the
/// type enum, composes a few thousand trees out of them, and asserts that every
/// leaf spelling those print appears somewhere in this list, and that no
/// spelling in this list has stopped being printed. `void` is the one exception,
/// and the reason is on the test.
///
/// Concrete instantiations stand in for the parametric shapes — `Array<T>` is
/// `{ ptr, i64, i64 }` whatever `T` is, but `SmallArray` and `ArrayN` embed the
/// element type, so those appear at several element widths.
pub const SHAPES: &[(&str, &str)] = &[
    // Scalars.
    ("Int64", "i64"),
    ("Int32", "i32"),
    ("Int16", "i16"),
    ("Int8", "i8"),
    ("Bool", "i1"),
    ("Float64", "double"),
    ("Float32", "float"),
    ("String", "ptr"),
    // The fixed aggregates. `Array` leads with a narrow member and then needs
    // i64 alignment, which is the interesting one. `Option`/`Result` used to sit
    // beside it as `{ i1, i64, i64 }`; RFC-0126 §8.4 gave every sum the enum's
    // `i64` tag and a slot count that follows the payload's width, so the sum
    // rows below are theirs too.
    ("Array", "{ ptr, i64, i64 }"),
    // A `Stream<T>` (RFC-0075 M2b): the array triple, then the producer's tag,
    // payload and cursor generation.
    ("Stream", "{ ptr, i64, i64, i64, i64, i64 }"),
    ("Map", "{ ptr, ptr, i64, i64, ptr }"),
    ("Ref", "{ i64, i64 }"),
    ("Fn", "{ i64, i64 }"),
    // The vectors (RFC-0083). Four Vyrn types under four spellings and one
    // machine shape — 16 bytes, 16-aligned — but written out separately because
    // the point of this list is what `llt` PRINTS, and clang is asked about each
    // spelling rather than told they agree.
    ("F32x4", "<4 x float>"),
    ("I32x4/Mask32x4", "<4 x i32>"),
    ("F64x2", "<2 x double>"),
    ("Mask64x2", "<2 x i64>"),
    // A vector inside a record, which is the padding case the bare vector cannot
    // show: 16-alignment pushes the member to 16 and the struct to 32.
    ("RecordOfVector", "{ i8, <4 x float> }"),
    // Sums: one i64 tag plus one i64 per payload SLOT of the widest variant
    // (RFC-0126 §8.4) — `Option<Int64>` and a one-word enum are `Enum1`,
    // `Option<fn(Int64)>` and a two-word enum are `Enum2`.
    ("Enum0", "{ i64 }"),
    ("Enum1", "{ i64, i64 }"),
    ("Enum2", "{ i64, i64, i64 }"),
    ("Enum3", "{ i64, i64, i64, i64 }"),
    // Records, including the mixed-width and nested cases. Nesting is the shape
    // the spike measured 19 deep; three levels exercises the same rule.
    ("RecordEmpty", "{  }"),
    ("RecordMixed", "{ i1, ptr, i64, i8, double }"),
    ("RecordNarrow", "{ i8, i8, i16 }"),
    ("RecordNested", "{ i8, { i8, { i8, i64 } }, i32 }"),
    ("RecordOfArray", "{ i8, { ptr, i64, i64 } }"),
    // Fixed-size arrays and the small-buffer array that embeds one. The i8 case
    // is the one where the inline buffer does not end on the struct's alignment.
    ("ArrayN_i64", "[4 x i64]"),
    ("ArrayN_i8", "[3 x i8]"),
    ("ArrayN_struct", "[2 x { i8, i64 }]"),
    ("SmallArray_i64", "{ i64, i64, ptr, [4 x i64] }"),
    ("SmallArray_i8", "{ i64, i64, ptr, [3 x i8] }"),
    ("SmallArray_str", "{ i64, i64, ptr, [2 x ptr] }"),
];

/// The layout of one LLVM type string, as `llt` prints it.
///
/// Errors rather than panics on anything outside that grammar: the input is
/// generated, so a rejection means this crate contradicts itself and the caller
/// wants to say which shape it choked on.
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
        self.i += 1; // '{'
                     // In 64 bits, because a member past 4 GB is a shape to REFUSE and a
                     // wrapped running total is one to accept by mistake.
        let (mut size, mut align, mut fields) = (0u64, 1u32, Vec::<u64>::new());
        if !self.eat(b'}') {
            loop {
                let f = self.ty()?;
                // Each member starts at the next multiple of its own alignment;
                // the hole before it is the padding.
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
        self.i += 1; // '['
        let n = self.count()?;
        if !self.s[self.i..].starts_with(b"x") {
            return Err(format!("expected `x` at byte {}", self.i));
        }
        self.i += 1;
        let elem = self.ty()?;
        if !self.eat(b']') {
            return Err(format!("expected `]` at byte {}", self.i));
        }
        // `elem.size` is already rounded to `elem.align` (every shape here is),
        // so it IS the stride.
        Ok(Layout {
            size: fits(n as u64 * elem.size as u64, "a fixed array")?,
            align: elem.align,
            fields: Vec::new(),
        })
    }

    /// `<N x T>` — LLVM's own vector (RFC-0083), the one shape whose bytes are
    /// not a struct rule.
    ///
    /// It needs a layout here for the same reason every other shape does: `repr`
    /// already reads `<4 x float>` back as a wasm `v128` VALUE, but a vector
    /// inside a record, an array or a `Map` is bytes in memory, and an aggregate
    /// containing one has no size until this does.
    ///
    /// Size is the lanes, unpadded. Alignment is that size rounded up to a power
    /// of two — LLVM's rule for a vector with no explicit alignment in the data
    /// layout, and clang's for `__attribute__((vector_size))`. All four spellings
    /// `llt` prints come out 16/16; the rule is written rather than the number so
    /// a fifth width would not need a fifth line.
    fn vector(&mut self) -> Result<Layout, String> {
        self.i += 1; // '<'
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

    /// The lane or element count leading an `[N x T]` / `<N x T>`, and the `x`'s
    /// leading whitespace with it.
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
            // A pointer is 4 bytes on wasm32 — the one place this differs from
            // the native build sharing the same `llt` output.
            "ptr" => (4, 4),
            "double" => (8, 8),
            "float" => (4, 4),
            // `void` has no storage; it only ever appears as a return type, so
            // it never contributes padding to anything.
            "void" => (0, 1),
            // `i1` occupies a whole byte in memory (LLVM's alloc size), which is
            // what decides where the `i64` after it in an Option lands.
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

/// `bytes` as the `u32` a size is, or the refusal that says why it is not one.
///
/// The limit is the type's, not a policy: every offset this engine hands out is
/// a `u32` and a wasm32 memory is exactly that wide. What matters is that the
/// check happens HERE. `Array<Int64, 600000000>` is 4.8 GB; wrapped, it is
/// 505,032,704, and every bound downstream would take that at face value — the
/// frame limit compares it, `malloc` allocates it, and `memory.copy` copies the
/// same product read as unsigned. A refusal at the measurement is the only place
/// the two numbers are still the same number.
fn fits(bytes: u64, what: &str) -> Result<u32, String> {
    u32::try_from(bytes).map_err(|_| {
        format!(
            "{what} needs {bytes} bytes, past the {} one shape may occupy; \
             a fixed array this big belongs on the heap as `Array<T>`",
            u32::MAX
        )
    })
}

/// `n` rounded up to a multiple of `align`, in 64 bits — the width the callers
/// accumulate in, so the rounding cannot be what overflows.
fn round_up(n: u64, align: u64) -> u64 {
    debug_assert!(align.is_power_of_two());
    (n + align - 1) & !(align - 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The four numbers the rest of the backend will be built on. Written out
    /// rather than derived so that a change to the parser has to disagree with
    /// something a human wrote down.
    #[test]
    fn the_shapes_that_cross_the_shim_boundary() {
        // `Array<T>` — and `__vyrn_args()`'s return type. The 4-byte hole after
        // the pointer is the whole wasm32 story.
        let a = of_ll("{ ptr, i64, i64 }").unwrap();
        assert_eq!((a.size, a.align, &a.fields[..]), (24, 8, &[0, 8, 16][..]));
        // `Map<String, V>` — the ONE aggregate the shim also declares, as `VMap`.
        // The fifth field is RFC-0104 work item 1's hash index, and it lands in
        // the tail padding an i64-aligned struct had anyway: 24 bytes became 32,
        // not 28.
        let m = of_ll("{ ptr, ptr, i64, i64, ptr }").unwrap();
        assert_eq!(
            (m.size, m.align, &m.fields[..]),
            (32, 8, &[0, 4, 8, 16, 24][..])
        );
        // A sum with two payload slots — `Option<fn(Int64)>`, or an enum whose
        // widest variant is two words wide (RFC-0126 §8.4).
        let o = of_ll("{ i64, i64, i64 }").unwrap();
        assert_eq!((o.size, o.align, &o.fields[..]), (24, 8, &[0, 8, 16][..]));
        // A defunctionalized `fn` value (RFC-0037): tag plus capture block.
        let r = of_ll("{ i64, i64 }").unwrap();
        assert_eq!((r.size, r.align, &r.fields[..]), (16, 8, &[0, 8][..]));
    }

    /// Tail padding is not decoration: a `SmallArray<UInt8, 3>` whose inline
    /// buffer ends at 23 must still be 24 bytes, or an array of them walks off
    /// its own alignment.
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

    /// All four vector spellings, and the record that shows their alignment
    /// doing something: 16 pushes the member off byte 1 and the struct to 32.
    #[test]
    fn a_vector_is_sixteen_bytes_sixteen_aligned() {
        for ll in ["<4 x float>", "<4 x i32>", "<2 x double>", "<2 x i64>"] {
            let v = of_ll(ll).unwrap_or_else(|e| panic!("{ll}: {e}"));
            assert_eq!((v.size, v.align), (16, 16), "{ll}");
        }
        let r = of_ll("{ i8, <4 x float> }").unwrap();
        assert_eq!((r.size, r.align, &r.fields[..]), (32, 16, &[0, 16][..]));
        // And in the two containers a vector reaches memory through.
        assert_eq!(of_ll("[3 x <2 x i64>]").unwrap().size, 48);
    }

    /// A shape past 4 GB is refused, not wrapped. `600000000 * 8` is the one
    /// that shipped: it is 4,800,000,000, and `as u32` makes it 505,032,704 —
    /// a number every bound downstream would accept.
    ///
    /// The exact-multiple case is the one that proves the refusal has to be
    /// here. `536870912 * 8` is 2^32, so the wrap is ZERO: a 4 GiB array that
    /// claims to need no bytes at all sails past the frame limit and gets a
    /// module written for it.
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
        // The largest shape that still fits is still described, exactly.
        assert_eq!(of_ll("[536870911 x i64]").unwrap().size, 4_294_967_288);
    }
}
