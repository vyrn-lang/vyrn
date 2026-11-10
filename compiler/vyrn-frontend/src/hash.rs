//! SHA-256 (FIPS 180-4) in std only, so the frontend pulls no crate for it. It
//! keys the generator cache and verifies every content-addressed
//! blob.

use std::fmt::Write as _;

const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

/// Returns the SHA-256 digest of `data` in lowercase hex.
pub fn sha256_hex(data: &[u8]) -> String {
    sha256_hex_of(&[data])
}

/// Returns the SHA-256 digest of the concatenation of `parts` in lowercase
/// hex. Whole blocks compress in place, so no part is copied.
pub fn sha256_hex_of(parts: &[&[u8]]) -> String {
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    // The bytes of the block in progress, then room for the padding.
    let mut tail = [0u8; 128];
    let (mut held, mut len) = (0, 0u64);
    for &part in parts {
        len += part.len() as u64;
        let take = part.len().min((64 - held) % 64);
        tail[held..held + take].copy_from_slice(&part[..take]);
        held += take;
        if held == 64 {
            compress(&mut h, &tail[..64]);
            held = 0;
        }
        let rest = &part[take..];
        let whole = rest.len() - rest.len() % 64;
        compress(&mut h, &rest[..whole]);
        tail[held..held + rest.len() - whole].copy_from_slice(&rest[whole..]);
        held += rest.len() - whole;
    }
    // The padding: a one bit, zeros, and the length in bits, to whole blocks.
    let end = if held < 56 { 64 } else { 128 };
    tail[held] = 0x80;
    tail[held + 1..end - 8].fill(0);
    tail[end - 8..end].copy_from_slice(&len.wrapping_mul(8).to_be_bytes());
    compress(&mut h, &tail[..end]);
    let mut hex = String::with_capacity(64);
    for x in h {
        let _ = write!(hex, "{x:08x}");
    }
    hex
}

/// Runs the compression function over each 64-byte block of `blocks`: with
/// the SHA extensions on an x86_64 CPU that has them, else with the portable
/// code, which every other target runs (aarch64, wasm).
fn compress(h: &mut [u32; 8], blocks: &[u8]) {
    #[cfg(target_arch = "x86_64")]
    if is_x86_feature_detected!("sha")
        && is_x86_feature_detected!("ssse3")
        && is_x86_feature_detected!("sse4.1")
    {
        // SAFETY: the three checks above detect every feature compress_ni
        // enables but sse2, which every x86_64 CPU has.
        return unsafe { compress_ni(h, blocks) };
    }
    compress_soft(h, blocks)
}

/// The compression function in portable code.
fn compress_soft(h: &mut [u32; 8], blocks: &[u8]) {
    for chunk in blocks.chunks_exact(64) {
        let mut w = [0u32; 64];
        for (i, word) in chunk.chunks_exact(4).enumerate() {
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let (mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh) =
            (h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7]);
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
        h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g);
        h[7] = h[7].wrapping_add(hh);
    }
}

/// Runs the SHA-256 compression function over each 64-byte block of `blocks`
/// with the x86 SHA extensions.
///
/// # Safety
///
/// The CPU must have the `sha`, `sse2`, `ssse3` and `sse4.1` features.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sha,sse2,ssse3,sse4.1")]
unsafe fn compress_ni(h: &mut [u32; 8], blocks: &[u8]) {
    use std::arch::x86_64::*;
    let k = |i: usize| _mm_loadu_si128(K.as_ptr().add(4 * i).cast());
    let mask = _mm_set_epi64x(0x0c0d_0e0f_0809_0a0b, 0x0405_0607_0001_0203);
    let dcba = _mm_loadu_si128(h.as_ptr().cast());
    let hgfe = _mm_loadu_si128(h.as_ptr().add(4).cast());
    let cdab = _mm_shuffle_epi32(dcba, 0xb1);
    let efgh = _mm_shuffle_epi32(hgfe, 0x1b);
    let mut abef = _mm_alignr_epi8(cdab, efgh, 8);
    let mut cdgh = _mm_blend_epi16(efgh, cdab, 0xf0);
    for block in blocks.chunks_exact(64) {
        let (abef0, cdgh0) = (abef, cdgh);
        let p = block.as_ptr().cast::<__m128i>();
        let mut w = [
            _mm_shuffle_epi8(_mm_loadu_si128(p), mask),
            _mm_shuffle_epi8(_mm_loadu_si128(p.add(1)), mask),
            _mm_shuffle_epi8(_mm_loadu_si128(p.add(2)), mask),
            _mm_shuffle_epi8(_mm_loadu_si128(p.add(3)), mask),
        ];
        for i in 0..16 {
            if i >= 4 {
                let t = _mm_add_epi32(
                    _mm_sha256msg1_epu32(w[i % 4], w[(i + 1) % 4]),
                    _mm_alignr_epi8(w[(i + 3) % 4], w[(i + 2) % 4], 4),
                );
                w[i % 4] = _mm_sha256msg2_epu32(t, w[(i + 3) % 4]);
            }
            let t = _mm_add_epi32(w[i % 4], k(i));
            cdgh = _mm_sha256rnds2_epu32(cdgh, abef, t);
            abef = _mm_sha256rnds2_epu32(abef, cdgh, _mm_shuffle_epi32(t, 0x0e));
        }
        abef = _mm_add_epi32(abef, abef0);
        cdgh = _mm_add_epi32(cdgh, cdgh0);
    }
    let feba = _mm_shuffle_epi32(abef, 0x1b);
    let dchg = _mm_shuffle_epi32(cdgh, 0xb1);
    _mm_storeu_si128(h.as_mut_ptr().cast(), _mm_blend_epi16(feba, dchg, 0xf0));
    _mm_storeu_si128(h.as_mut_ptr().add(4).cast(), _mm_alignr_epi8(dchg, feba, 8));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_nist_vectors() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256_hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
        // A length crossing the 55-byte padding boundary.
        assert_eq!(
            sha256_hex(&[b'a'; 64]),
            "ffe054fe7ae0cb6dc65c3af9b61d5209f439851db43d0ba5997337df154668eb"
        );
    }

    #[test]
    fn a_split_hashes_as_the_whole() {
        let data: Vec<u8> = (0..300u32).map(|i| (i * 7) as u8).collect();
        for cut in [0, 1, 55, 56, 63, 64, 65, 127, 128, 200, 300] {
            let (a, b) = data.split_at(cut);
            let (b, c) = b.split_at(b.len() / 3);
            assert_eq!(sha256_hex_of(&[a, b, c]), sha256_hex(&data), "cut at {cut}");
        }
    }

    #[test]
    fn the_extensions_compress_each_block_as_the_portable_code_does() {
        let mut x: u64 = 0x9e37_79b9_7f4a_7c15;
        let random: Vec<u8> = (0..1 << 20)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x as u8
            })
            .collect();
        let inputs: [&[u8]; 2] = [&[b'a'; 64], &random];
        for input in inputs {
            let (mut a, mut b) = ([0x6a09e667u32; 8], [0x6a09e667u32; 8]);
            for (i, block) in input.chunks_exact(64).enumerate() {
                compress(&mut a, block);
                compress_soft(&mut b, block);
                assert_eq!(a, b, "block {i} of {} bytes", input.len());
            }
        }
    }
}
