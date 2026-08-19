// binary-trees, from the Computer Language Benchmarks Game.
//
// The Rust leg of RFC-0104 M2. It computes what `examples/binarytrees.vyrn`
// computes and prints the same bytes.
//
//   $ rustc -C opt-level=3 -o binarytrees.exe binarytrees.rs
//   $ ./binarytrees 10
//
// Safe Rust, std only: `Box` for the nodes and `Drop` for the release, which is
// what the benchmark is there to measure. No arena, no pool.

use std::env;

/// The census N — depth 10.
const ORDER: i64 = 10;

/// The game's tree: no payload, because the benchmark is about the nodes and
/// not about what they hold.
enum Tree {
    Leaf,
    Node(Box<Tree>, Box<Tree>),
}

/// A complete tree of `depth`.
fn make(depth: i64) -> Tree {
    if depth == 0 {
        return Tree::Leaf;
    }
    Tree::Node(Box::new(make(depth - 1)), Box::new(make(depth - 1)))
}

/// The node count — the game's checksum.
fn check(t: &Tree) -> i64 {
    match t {
        Tree::Leaf => 1,
        Tree::Node(l, r) => 1 + check(l) + check(r),
    }
}

/// `iterations` trees of `depth`, built, checked and released one at a time.
fn check_all(depth: i64, iterations: i64) -> i64 {
    let mut sum = 0;
    for _ in 0..iterations {
        sum = sum + check(&make(depth));
    }
    sum
}

/// How many trees of `depth` the game asks for at this `max_depth`.
fn iterations_for(depth: i64, max_depth: i64, min_depth: i64) -> i64 {
    let mut n = 1;
    let mut s = 0;
    while s < max_depth - depth + min_depth {
        n = n * 2;
        s = s + 1;
    }
    n
}

/// The whole run at `n`: the stretch tree, one line per even depth, and the
/// long-lived tree that stays alive across all of them.
fn run(n: i64) {
    let min_depth = 4;
    let mut max_depth = n;
    if max_depth < min_depth + 2 {
        max_depth = min_depth + 2;
    }
    let stretch_depth = max_depth + 1;

    println!(
        "stretch tree of depth {}\t check: {}",
        stretch_depth,
        check(&make(stretch_depth))
    );
    let long_lived = make(max_depth);

    let mut depth = min_depth;
    while depth < stretch_depth {
        let iterations = iterations_for(depth, max_depth, min_depth);
        println!(
            "{}\t trees of depth {}\t check: {}",
            iterations,
            depth,
            check_all(depth, iterations)
        );
        depth = depth + 2;
    }
    println!(
        "long lived tree of depth {}\t check: {}",
        max_depth,
        check(&long_lived)
    );
}

fn main() {
    let n: i64 = env::args()
        .nth(1)
        .map(|a| a.parse().expect("N must be an integer"))
        .unwrap_or(ORDER);

    run(n);
}
