// dlist (idiomatic) — same protocol, links are generational slotmap keys.
// Unlike the std twin's raw indices, a key held after removal MISSES
// deterministically instead of silently reading a reused slot — the same
// behavior class as Jestyr's genrefs, delivered by a crate instead of
// the language. Removal genuinely frees the slot.
//
// Output must be byte-identical to the std-only and Jestyr twins.

use slotmap::{DefaultKey, SlotMap};

const N: i64 = 200_000;

struct DNode {
    v: i64,
    prev: Option<DefaultKey>,
    next: Option<DefaultKey>,
}

fn lcg(state: i64) -> i64 {
    (state * 48271) % 2147483647
}

fn main() {
    let mut nodes: SlotMap<DefaultKey, DNode> = SlotMap::with_capacity((N + N / 5) as usize);
    let mut s: i64 = 20260813;

    s = lcg(s);
    let head = nodes.insert(DNode { v: s % 1000, prev: None, next: None });
    let mut tail = head;
    let mut i: i64 = 1;
    while i < N {
        s = lcg(s);
        let id = nodes.insert(DNode { v: s % 1000, prev: Some(tail), next: None });
        nodes[tail].next = Some(id);
        tail = id;
        i += 1;
    }
    let mut live: i64 = N;

    let mut f1: i64 = 0;
    let mut cur = Some(head);
    while let Some(c) = cur {
        f1 += nodes[c].v;
        cur = nodes[c].next;
    }

    let mut k: i64 = 0;
    cur = Some(head);
    while let Some(c) = cur {
        k += 1;
        let nx = nodes[c].next;
        if k % 3 == 0 && nx.is_some() {
            let pv = nodes[c].prev;
            if let Some(p) = pv {
                nodes[p].next = nx;
            }
            nodes[nx.unwrap()].prev = pv;
            nodes.remove(c);
            live -= 1;
        }
        cur = nx;
    }

    let mut b1: i64 = 0;
    cur = Some(tail);
    while let Some(c) = cur {
        b1 += nodes[c].v;
        cur = nodes[c].prev;
    }

    let mut m: i64 = 0;
    cur = Some(head);
    while let Some(c) = cur {
        m += 1;
        let nx = nodes[c].next;
        if m % 5 == 0 {
            s = lcg(s);
            let id = nodes.insert(DNode { v: s % 1000, prev: Some(c), next: nx });
            nodes[c].next = Some(id);
            if let Some(x) = nx {
                nodes[x].prev = Some(id);
            }
            live += 1;
        }
        cur = nx;
    }

    let mut f2: i64 = 0;
    cur = Some(head);
    while let Some(c) = cur {
        f2 += nodes[c].v;
        cur = nodes[c].next;
    }

    println!("{}", f1);
    println!("{}", b1);
    println!("{}", f2);
    println!("{}", live);
}
