//! The fixpoint driver every whole-program analysis over the call graph
//! runs on: [`solve`].

/// Solves every body's value to a fixpoint over the call graph. `values` and
/// `callees` hold one entry per body; `callees[i]` lists by index the bodies
/// body `i` calls.
///
/// Bodies are visited by strongly connected component, callees first. Each
/// round of a component visits its bodies in index order and runs
/// `join(&mut values[i], transfer(i, &values), round)`, with `round` from 0.
/// A component settles when every join of a round returns false; one with no
/// cycle runs one round. The result is indexed as `values` is.
///
/// The caller guarantees that `transfer(i, ..)` reads only body `i` and its
/// callees, so a callee's value is final before its caller reads it. It also
/// guarantees termination: `transfer` is monotone, and `join` returns true
/// only when it raised `values[i]`, in a lattice of finite height or by
/// widening once `round` passes a bound.
///
/// # Panics
///
/// If `values` is shorter than `callees` or an index in `callees` is not a
/// body.
pub(crate) fn solve<V>(
    mut values: Vec<V>,
    callees: &[Vec<usize>],
    mut transfer: impl FnMut(usize, &[V]) -> V,
    mut join: impl FnMut(&mut V, V, usize) -> bool,
) -> Vec<V> {
    for comp in components(callees) {
        let cyclic = comp.len() > 1 || callees[comp[0]].contains(&comp[0]);
        let mut round = 0;
        loop {
            let mut changed = false;
            for &i in &comp {
                let v = transfer(i, &values);
                changed |= join(&mut values[i], v, round);
            }
            round += 1;
            if !(changed && cyclic) {
                break;
            }
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
    use super::{components, solve};

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
        let own: Vec<u32> = (0..4).map(|i| 1 << i).collect();
        let reach = solve(
            own,
            &callees,
            |i, v| callees[i].iter().fold(0, |a, &j| a | v[j]),
            |old, new, _| {
                let before = *old;
                *old |= new;
                *old != before
            },
        );
        assert_eq!(reach, vec![0b1111, 0b1110, 0b1110, 0b1000]);
    }

    #[test]
    fn a_widening_ends_an_infinite_ascent() {
        // Each turn of the cycle adds one: the plain join never settles.
        let callees = vec![vec![1], vec![0]];
        let out = solve(
            vec![0u64, 0],
            &callees,
            |i, v| v[callees[i][0]].saturating_add(1),
            |old, new, round| {
                let next = if round >= 3 { u64::MAX } else { new.max(*old) };
                let changed = next != *old;
                *old = next;
                changed
            },
        );
        assert_eq!(out, vec![u64::MAX, u64::MAX]);
    }
}
