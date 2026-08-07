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

/// Where one shape's bytes are: its size, its alignment, and the offset of each
/// field (empty for scalars; for `[N x T]` the stride is `size / N`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    pub size: u32,
    pub align: u32,
    pub fields: Vec<u32>,
}

/// Every shape `llt` can produce, paired with a name for diagnostics. The
/// emitter's whole type universe, in one list, because the clang comparison has
/// to know what to compare and a hand-kept list in the test would rot. Kept
/// honest by `llt_covers_every_shape` in this crate's tests, which asserts these
/// strings are the ones `llt` actually prints.
///
/// Concrete instantiations stand in for the parametric shapes — `Array<T>` is
/// `{ ptr, i64, i64 }` whatever `T` is, but `SmallArray` and `ArrayN` embed the
/// element type, so those appear at several element widths. The point is to
/// cover the padding cases, not to enumerate programs.
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
    // The fixed aggregates. `Option`/`Result` and `Array` are the two that lead
    // with a narrow member and then need i64 alignment — the interesting ones.
    ("Option/Result", "{ i1, i64, i64 }"),
    ("Array", "{ ptr, i64, i64 }"),
    // A `Stream<T>` (RFC-0075 M2b): the array triple, then the producer's tag,
    // payload and cursor generation.
    ("Stream", "{ ptr, i64, i64, i64, i64, i64 }"),
    ("Map", "{ ptr, ptr, i64, i64 }"),
    ("Ref", "{ i64, i64 }"),
    ("Fn", "{ i64, i64 }"),
    // Enums: one i64 tag plus one i64 per payload slot of the widest variant.
    ("Enum0", "{ i64 }"),
    ("Enum1", "{ i64, i64 }"),
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
            Some(_) => self.scalar(),
            None => Err("unexpected end of type".to_string()),
        }
    }

    fn strukt(&mut self) -> Result<Layout, String> {
        self.i += 1; // '{'
        let (mut size, mut align, mut fields) = (0u32, 1u32, Vec::new());
        if !self.eat(b'}') {
            loop {
                let f = self.ty()?;
                // Each member starts at the next multiple of its own alignment;
                // the hole before it is the padding.
                size = round_up(size, f.align);
                fields.push(size);
                size += f.size;
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
        Ok(Layout {
            size: round_up(size, align),
            align,
            fields,
        })
    }

    fn array(&mut self) -> Result<Layout, String> {
        self.i += 1; // '['
        self.ws();
        let start = self.i;
        while self.s.get(self.i).is_some_and(u8::is_ascii_digit) {
            self.i += 1;
        }
        let n: u32 = std::str::from_utf8(&self.s[start..self.i])
            .ok()
            .and_then(|d| d.parse().ok())
            .ok_or_else(|| format!("expected an element count at byte {start}"))?;
        self.ws();
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
            size: n * elem.size,
            align: elem.align,
            fields: Vec::new(),
        })
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

fn round_up(n: u32, align: u32) -> u32 {
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
        let m = of_ll("{ ptr, ptr, i64, i64 }").unwrap();
        assert_eq!(
            (m.size, m.align, &m.fields[..]),
            (24, 8, &[0, 4, 8, 16][..])
        );
        // Option/Result: the `i1` is a byte, then 7 bytes of hole.
        let o = of_ll("{ i1, i64, i64 }").unwrap();
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
    }
}
