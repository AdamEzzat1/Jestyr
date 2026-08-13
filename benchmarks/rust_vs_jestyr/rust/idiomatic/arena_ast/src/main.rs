// arena_ast (idiomatic) — the same tree, but with REAL references out of
// `typed_arena`: children are `&'a Node<'a>`, parent back-links go through
// `Cell` because the node is shared once linked. This is where named
// lifetimes finally appear in this suite — the arena lifetime threads
// through every signature — and where interior mutability becomes the
// price of back-edges among immutable references.
//
// Output must be byte-identical to the std-only and Jestyr twins.

use std::cell::Cell;
use typed_arena::Arena;

const LEAVES: usize = 524_288;
const MOD: i64 = 1_000_000_007;

struct Node<'a> {
    kind: i64, // 0 = leaf, 1 = add, 2 = mul
    val: i64,
    left: Option<&'a Node<'a>>,
    right: Option<&'a Node<'a>>,
    parent: Cell<Option<&'a Node<'a>>>,
}

fn lcg(state: i64) -> i64 {
    (state * 48271) % 2147483647
}

fn eval(n: &Node) -> i64 {
    if n.kind == 0 {
        n.val
    } else {
        let a = eval(n.left.unwrap());
        let b = eval(n.right.unwrap());
        if n.kind == 1 { (a + b) % MOD } else { (a * b) % MOD }
    }
}

fn path_len(start: &Node) -> i64 {
    let mut d: i64 = 0;
    let mut cur = start;
    loop {
        d += 1;
        match cur.parent.get() {
            None => break,
            Some(p) => cur = p,
        }
    }
    d
}

fn main() {
    let arena: Arena<Node> = Arena::with_capacity(LEAVES * 2 - 1);
    let mut s: i64 = 20260813;
    let mut count: usize = 0;

    let mut leaves: Vec<&Node> = Vec::with_capacity(LEAVES);
    for _ in 0..LEAVES {
        s = lcg(s);
        leaves.push(arena.alloc(Node {
            kind: 0,
            val: s % 100,
            left: None,
            right: None,
            parent: Cell::new(None),
        }));
        count += 1;
    }

    let mut level: Vec<&Node> = leaves.clone();
    while level.len() > 1 {
        let half = level.len() / 2;
        let mut next: Vec<&Node> = Vec::with_capacity(half);
        for j in 0..half {
            let lc = level[j * 2];
            let rc = level[j * 2 + 1];
            s = lcg(s);
            let kind = s % 2 + 1;
            let p: &Node = arena.alloc(Node {
                kind,
                val: 0,
                left: Some(lc),
                right: Some(rc),
                parent: Cell::new(None),
            });
            count += 1;
            lc.parent.set(Some(p));
            rc.parent.set(Some(p));
            next.push(p);
        }
        level = next;
    }
    let root = level[0];

    let mut wsum: i64 = 0;
    let mut i: usize = 0;
    while i < LEAVES {
        wsum += path_len(leaves[i]) * ((i as i64) % 13 + 1);
        i += 1;
    }

    println!("{}", eval(root));
    println!("{}", wsum);
    println!("{}", count);
}
