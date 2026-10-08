//! The fixpoint drivers every whole-program analysis over the call graph
//! runs on: [`solve`] raises values to the least fixpoint, [`descend`] lowers
//! them to the greatest.

use std::collections::BTreeSet;

/// A join semilattice of finite height: the value an analysis keeps per body.
pub(crate) trait Lattice {
    /// Raises `self` to its join with `other` and returns whether `self`
    /// grew. A value that never grows past a finite height ends every loop.
    fn join(&mut self, other: &Self) -> bool;
}

/// Solves every body's value to the least fixpoint of
/// `values[i] = values[i] join (join of values[j] for j in callees[i])`.
/// `values` and `callees` hold one entry per body; `callees[i]` lists by
/// index the bodies body `i` calls, and `values` starts at each body's own
/// value.
///
/// Bodies are visited by strongly connected component, callees first. Each
/// round of a component joins every callee into each body in index order. A
/// component settles when a round grows nothing; one with no cycle runs one
/// round. The result is indexed as `values` is.
///
/// # Panics
///
/// If `values` is shorter than `callees` or an index in `callees` is not a
/// body.
pub(crate) fn solve<L: Lattice>(mut values: Vec<L>, callees: &[Vec<usize>]) -> Vec<L> {
    for comp in components(callees) {
        let cyclic = comp.len() > 1 || callees[comp[0]].contains(&comp[0]);
        loop {
            let mut changed = false;
            for &i in &comp {
                for &j in callees[i].iter().filter(|&&j| j != i) {
                    let (this, callee) = if i < j {
                        let (lo, hi) = values.split_at_mut(j);
                        (&mut lo[i], &hi[0])
                    } else {
                        let (lo, hi) = values.split_at_mut(i);
                        (&mut hi[0], &lo[j])
                    };
                    changed |= this.join(callee);
                }
            }
            if !(changed && cyclic) {
                break;
            }
        }
    }
    values
}

/// Lowers every body's value until no step lowers one: the greatest fixpoint
/// over a finite set of candidates. `step(i, values)` recomputes body `i`'s
/// value from the others' and returns every body whose value it lowered;
/// `deps[i]` lists the bodies to visit again when body `i`'s value lowers.
/// Every body is visited at least once, and each visit takes the least
/// pending index, so the order is the source's.
///
/// Each visit removes a body from the pending set and adds bodies only when a
/// value lowered. A value lowers by dropping a candidate, so the count of
/// live candidates, then the pending count, decrease: no round cap is needed.
///
/// # Panics
///
/// If `deps` is shorter than `values` or a step returns an index that is not
/// a body.
pub(crate) fn descend<L>(
    mut values: Vec<L>,
    deps: &[Vec<usize>],
    mut step: impl FnMut(usize, &mut [L]) -> Vec<usize>,
) -> Vec<L> {
    let mut pending: BTreeSet<usize> = (0..values.len()).collect();
    while let Some(i) = pending.pop_first() {
        for j in step(i, &mut values) {
            pending.extend(deps[j].iter().copied());
        }
    }
    values
}

/// Returns the strongly connected components of `callees`, each sorted, a
/// component after every component it reaches (Tarjan's algorithm, with an
/// explicit stack so a deep call chain cannot overflow the native one).
fn components(callees: &[Vec<usize>]) -> Vec<Vec<usize>> {
    const UNSEEN: usize = usize::MAX;
    let n = callees.len();
    let mut index = vec![UNSEEN; n];
    let mut low = vec![0; n];
    let mut on_stack = vec![false; n];
    let mut stack: Vec<usize> = Vec::new();
    let mut out: Vec<Vec<usize>> = Vec::new();
    let mut next = 0;
    for root in 0..n {
        if index[root] != UNSEEN {
            continue;
        }
        // (body, position of its next callee)
        let mut work = vec![(root, 0)];
        index[root] = next;
        low[root] = next;
        next += 1;
        stack.push(root);
        on_stack[root] = true;
        while let Some(top) = work.last_mut() {
            let (v, e) = *top;
            if let Some(&w) = callees[v].get(e) {
                top.1 += 1;
                if index[w] == UNSEEN {
                    index[w] = next;
                    low[w] = next;
                    next += 1;
                    stack.push(w);
                    on_stack[w] = true;
                    work.push((w, 0));
                } else if on_stack[w] {
                    low[v] = low[v].min(index[w]);
                }
                continue;
            }
            work.pop();
            if let Some(&(u, _)) = work.last() {
                low[u] = low[u].min(low[v]);
            }
            if low[v] == index[v] {
                let at = (stack.iter().rposition(|&x| x == v))
                    .expect("a component's root stays on the stack until it is closed");
                let mut comp = stack.split_off(at);
                for &x in &comp {
                    on_stack[x] = false;
                }
                comp.sort_unstable();
                out.push(comp);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{components, descend, solve, Lattice};

    /// A bit set, joined by union.
    struct Bits(u32);

    impl Lattice for Bits {
        fn join(&mut self, other: &Bits) -> bool {
            let before = self.0;
            self.0 |= other.0;
            self.0 != before
        }
    }

    #[test]
    fn a_component_follows_every_component_it_calls() {
        // 0 -> 1 <-> 2 -> 3, and 4 alone
        let callees = vec![vec![1], vec![2], vec![1, 3], vec![], vec![]];
        assert_eq!(
            components(&callees),
            vec![vec![3], vec![1, 2], vec![0], vec![4]]
        );
    }

    #[test]
    fn a_cycle_joins_to_the_least_fixpoint() {
        // Reachable bodies, as a bit set per body.
        let callees = vec![vec![1], vec![2], vec![1, 3], vec![]];
        let reach = solve((0..4).map(|i| Bits(1 << i)).collect(), &callees);
        let reach: Vec<u32> = reach.into_iter().map(|b| b.0).collect();
        assert_eq!(reach, vec![0b1111, 0b1110, 0b1110, 0b1000]);
    }

    #[test]
    fn a_lowered_value_revisits_its_dependents() {
        // Body i keeps the bits of its callee (i + 1) and its own mask; 3
        // has no callee. Callers are listed first, so 0 and 1 settle only
        // after 3 lowers and its drop reaches them through `deps`.
        let masks = [0b111, 0b111, 0b111, 0b001];
        let deps = vec![vec![], vec![0], vec![1], vec![2]];
        let out = descend(vec![0b111u32; 4], &deps, |i, v| {
            let below = v.get(i + 1).copied().unwrap_or(u32::MAX);
            let new = v[i] & masks[i] & below;
            let lowered = new != v[i];
            v[i] = new;
            if lowered {
                vec![i]
            } else {
                vec![]
            }
        });
        assert_eq!(out, vec![0b001; 4]);
    }
}
