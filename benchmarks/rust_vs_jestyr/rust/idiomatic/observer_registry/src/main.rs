// observer_registry (idiomatic) — the same protocol as the std-only twin,
// but the generational arena is what a real Rust user would reach for:
// the `slotmap` crate. Keys are generational by construction; stale keys
// simply miss. The entire hand-rolled Registry from the std twin
// disappears into `SlotMap::with_key`.
//
// Output must be byte-identical to the std-only and Jestyr twins.

use slotmap::{DefaultKey, SlotMap};

const INITIAL: i64 = 100_000;
const ROUNDS: i64 = 20;
const DELETES: i64 = 30_000;
const SPAWNS: i64 = 15_000;

struct Obj {
    id: i64,
    hp: i64,
}

fn lcg(state: i64) -> i64 {
    (state * 48271) % 2147483647
}

fn main() {
    let mut reg: SlotMap<DefaultKey, Obj> = SlotMap::new();
    let mut handles: Vec<DefaultKey> = Vec::new();
    let mut s: i64 = 20260813;
    let mut next_id: i64 = 0;

    let mut i: i64 = 0;
    while i < INITIAL {
        s = lcg(s);
        handles.push(reg.insert(Obj { id: next_id, hp: s % 100 + 1 }));
        next_id += 1;
        i += 1;
    }

    let mut chk: i64 = 0;
    let mut dead_deletes: i64 = 0;
    let mut r: i64 = 0;
    while r < ROUNDS {
        let mut d: i64 = 0;
        while d < DELETES {
            s = lcg(s);
            let j = (s % handles.len() as i64) as usize;
            if reg.remove(handles[j]).is_none() {
                dead_deletes += 1;
            }
            d += 1;
        }
        let mut a: i64 = 0;
        while a < SPAWNS {
            s = lcg(s);
            handles.push(reg.insert(Obj { id: next_id, hp: s % 100 + 1 }));
            next_id += 1;
            a += 1;
        }
        let mut live_sum: i64 = 0;
        let mut live_count: i64 = 0;
        let mut stale_count: i64 = 0;
        for &h in &handles {
            match reg.get(h) {
                Some(o) => {
                    live_sum += o.hp + o.id % 7;
                    live_count += 1;
                }
                None => stale_count += 1,
            }
        }
        chk = (chk * 31 + live_sum + live_count * 13 + stale_count * 17) % 1_000_000_007;
        r += 1;
    }

    println!("{}", chk);
    println!("{}", handles.len());
    println!("{}", dead_deletes);
}
