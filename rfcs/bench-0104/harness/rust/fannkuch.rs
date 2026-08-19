// fannkuch-redux, from the Computer Language Benchmarks Game.
//
// The Rust leg of RFC-0104 M2. It computes what `examples/fannkuch.vyrn`
// computes and prints the same bytes.
//
//   $ rustc -C opt-level=3 -o fannkuch.exe fannkuch.rs
//   $ ./fannkuch 7
//
// Safe Rust, std only. Single threaded, like the Vyrn program.

use std::env;

/// The census N. The work is `n!`.
const ORDER: i64 = 7;

struct Fold {
    checksum: i64,
    maxflips: i64,
}

/// Reverse `p[0 ..= k]` in place — the flip the benchmark counts.
fn flip(a: &mut [i64], k: usize) {
    a[..=k].reverse();
}

/// How many flips `p` takes to bring a 0 to the front. `p` is scratch and is
/// left permuted.
fn fold_count(a: &mut [i64]) -> i64 {
    let mut flips = 0;
    let mut k = a[0];
    while k != 0 {
        flip(a, k as usize);
        flips = flips + 1;
        k = a[0];
    }
    flips
}

/// The alternating-sign checksum and the deepest fold, over every permutation of
/// `n` elements in the game's prescribed order.
fn fannkuch(n: i64) -> Fold {
    let mut perm1: Vec<i64> = (0..n).collect();
    let mut scratch: Vec<i64> = Vec::with_capacity(n as usize);
    let mut count = vec![0i64; n as usize];
    let mut maxflips = 0;
    let mut checksum = 0;
    let mut permcount = 0;
    let mut r = n;
    let mut done = false;
    while !done {
        while r != 1 {
            count[(r - 1) as usize] = r;
            r = r - 1;
        }
        // `clone_from` reuses the scratch buffer's allocation, so the walk does
        // not allocate per permutation.
        scratch.clone_from(&perm1);
        let flips = fold_count(&mut scratch);
        if flips > maxflips {
            maxflips = flips;
        }
        if permcount % 2 == 0 {
            checksum = checksum + flips;
        } else {
            checksum = checksum - flips;
        }
        permcount = permcount + 1;
        // The next permutation, by rotating the first `r + 1` entries left and
        // carrying into the next position when a rotation runs out.
        let mut advanced = false;
        while !advanced && !done {
            if r == n {
                done = true;
            } else {
                let first = perm1[0];
                for m in 0..r as usize {
                    perm1[m] = perm1[m + 1];
                }
                perm1[r as usize] = first;
                count[r as usize] = count[r as usize] - 1;
                if count[r as usize] > 0 {
                    advanced = true;
                } else {
                    r = r + 1;
                }
            }
        }
    }
    Fold { checksum, maxflips }
}

fn main() {
    let n: i64 = env::args()
        .nth(1)
        .map(|a| a.parse().expect("N must be an integer"))
        .unwrap_or(ORDER);

    let f = fannkuch(n);
    println!("{}", f.checksum);
    println!("Pfannkuchen({}) = {}", n, f.maxflips);
}
