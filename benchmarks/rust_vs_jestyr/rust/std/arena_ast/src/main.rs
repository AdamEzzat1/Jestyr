// arena_ast (std-only) — an expression tree with parent back-links, built
// bottom-up as a balanced tournament, then folded twice (eval + weighted
// depth). The ownership property under test: cross-linked node graphs.
// Std-only Rust sidesteps the borrow checker entirely by using INDICES
// into one Vec — no references, no lifetimes, and also no protection:
// a stale or wrong index is a logic bug the type system cannot see.
//
// Output must be byte-identical to the idiomatic and Jestyr twins.

const LEAVES: usize = 524_288; // 2^19 -> 1,048,575 nodes, depth 19
const MOD: i64 = 1_000_000_007;
const NIL: u32 = u32::MAX;

struct Node {
    kind: i64, // 0 = leaf, 1 = add, 2 = mul
    val: i64,
    left: u32,
    right: u32,
    parent: u32,
}

fn lcg(state: i64) -> i64 {
    (state * 48271) % 2147483647
}

fn eval(nodes: &[Node], i: u32) -> i64 {
    let n = &nodes[i as usize];
    if n.kind == 0 {
        n.val
    } else {
        let a = eval(nodes, n.left);
        let b = eval(nodes, n.right);
        if n.kind == 1 { (a + b) % MOD } else { (a * b) % MOD }
    }
}

// Path length in nodes (leaf included) from `i` up to the root.
fn path_len(nodes: &[Node], start: u32) -> i64 {
    let mut d: i64 = 0;
    let mut i = start;
    loop {
        d += 1;
        let p = nodes[i as usize].parent;
        if p == NIL {
            break;
        }
        i = p;
    }
    d
}

fn main() {
    let mut nodes: Vec<Node> = Vec::with_capacity(LEAVES * 2 - 1);
    let mut s: i64 = 20260813;

    let mut level: Vec<u32> = Vec::with_capacity(LEAVES);
    for _ in 0..LEAVES {
        s = lcg(s);
        nodes.push(Node { kind: 0, val: s % 100, left: NIL, right: NIL, parent: NIL });
        level.push((nodes.len() - 1) as u32);
    }

    while level.len() > 1 {
        let half = level.len() / 2;
        let mut next: Vec<u32> = Vec::with_capacity(half);
        for j in 0..half {
            let lc = level[j * 2];
            let rc = level[j * 2 + 1];
            s = lcg(s);
            let kind = s % 2 + 1;
            nodes.push(Node { kind, val: 0, left: lc, right: rc, parent: NIL });
            let p = (nodes.len() - 1) as u32;
            nodes[lc as usize].parent = p;
            nodes[rc as usize].parent = p;
            next.push(p);
        }
        level = next;
    }
    let root = level[0];

    let mut wsum: i64 = 0;
    let mut i: usize = 0;
    while i < LEAVES {
        wsum += path_len(&nodes, i as u32) * ((i as i64) % 13 + 1);
        i += 1;
    }

    println!("{}", eval(&nodes, root));
    println!("{}", wsum);
    println!("{}", nodes.len());
}
