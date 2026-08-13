// dlist (std-only) — the intentionally hard case: a doubly linked list
// with mid-walk deletion and insertion, forward and backward traversal.
// Safe std Rust cannot write pointer-linked nodes without Rc<RefCell<_>>
// runtime ceremony, so the idiomatic safe answer is INDICES into a slot
// vector: prev/next are u32 slots, NIL is a sentinel. No unsafe, no
// lifetimes — and no protection either: a stale index silently reads
// whatever lives in that slot now. Deleted slots are unlinked but not
// reused (simplification; the slotmap and Jestyr twins genuinely free).
//
// Output must be byte-identical to the idiomatic and Jestyr twins.

const N: i64 = 200_000;
const NIL: u32 = u32::MAX;

struct DNode {
    v: i64,
    prev: u32,
    next: u32,
}

fn lcg(state: i64) -> i64 {
    (state * 48271) % 2147483647
}

fn main() {
    let mut nodes: Vec<DNode> = Vec::with_capacity((N + N / 5) as usize);
    let mut s: i64 = 20260813;

    // Build: push_back N nodes.
    s = lcg(s);
    nodes.push(DNode { v: s % 1000, prev: NIL, next: NIL });
    let head: u32 = 0;
    let mut tail: u32 = 0;
    let mut i: i64 = 1;
    while i < N {
        s = lcg(s);
        nodes.push(DNode { v: s % 1000, prev: tail, next: NIL });
        let id = (nodes.len() - 1) as u32;
        nodes[tail as usize].next = id;
        tail = id;
        i += 1;
    }
    let mut live: i64 = N;

    // Forward sum.
    let mut f1: i64 = 0;
    let mut cur = head;
    while cur != NIL {
        f1 += nodes[cur as usize].v;
        cur = nodes[cur as usize].next;
    }

    // Delete every 3rd visited node (tail guarded: never deleted).
    let mut k: i64 = 0;
    cur = head;
    while cur != NIL {
        k += 1;
        let nx = nodes[cur as usize].next;
        if k % 3 == 0 && nx != NIL {
            let pv = nodes[cur as usize].prev;
            if pv != NIL {
                nodes[pv as usize].next = nx;
            }
            nodes[nx as usize].prev = pv;
            live -= 1;
        }
        cur = nx;
    }

    // Backward sum from the (never-deleted) tail.
    let mut b1: i64 = 0;
    cur = tail;
    while cur != NIL {
        b1 += nodes[cur as usize].v;
        cur = nodes[cur as usize].prev;
    }

    // Insert a fresh node after every 5th surviving node; skip past it.
    let mut m: i64 = 0;
    cur = head;
    while cur != NIL {
        m += 1;
        let nx = nodes[cur as usize].next;
        if m % 5 == 0 {
            s = lcg(s);
            nodes.push(DNode { v: s % 1000, prev: cur, next: nx });
            let id = (nodes.len() - 1) as u32;
            nodes[cur as usize].next = id;
            if nx != NIL {
                nodes[nx as usize].prev = id;
            }
            live += 1;
        }
        cur = nx;
    }

    // Final forward sum and count.
    let mut f2: i64 = 0;
    cur = head;
    while cur != NIL {
        f2 += nodes[cur as usize].v;
        cur = nodes[cur as usize].next;
    }

    println!("{}", f1);
    println!("{}", b1);
    println!("{}", f2);
    println!("{}", live);
}
