// observer_registry (std-only) — hold handles to objects that get deleted
// out from under you, then dereference every handle ever issued. The
// safety property under test: a stale handle must be DETECTED, not
// dereferenced into reused memory. Std-only Rust has no generational map,
// so this file hand-rolls one: slots carry a generation counter, handles
// carry the generation they were issued under, and a mismatch means stale.
//
// Output must be byte-identical to the idiomatic (slotmap) and Jestyr twins.

const INITIAL: i64 = 100_000;
const ROUNDS: i64 = 20;
const DELETES: i64 = 30_000;
const SPAWNS: i64 = 15_000;

struct Obj {
    id: i64,
    hp: i64,
}

#[derive(Clone, Copy)]
struct Handle {
    idx: usize,
    generation: i64,
}

struct Slot {
    generation: i64,
    val: Option<Obj>,
}

struct Registry {
    slots: Vec<Slot>,
    free: Vec<usize>,
}

impl Registry {
    fn new() -> Registry {
        Registry { slots: Vec::new(), free: Vec::new() }
    }

    fn insert(&mut self, obj: Obj) -> Handle {
        match self.free.pop() {
            Some(idx) => {
                self.slots[idx].val = Some(obj);
                Handle { idx, generation: self.slots[idx].generation }
            }
            None => {
                self.slots.push(Slot { generation: 0, val: Some(obj) });
                Handle { idx: self.slots.len() - 1, generation: 0 }
            }
        }
    }

    // Stale handles fall out naturally: the generation no longer matches.
    fn get(&self, h: Handle) -> Option<&Obj> {
        let slot = &self.slots[h.idx];
        if slot.generation == h.generation { slot.val.as_ref() } else { None }
    }

    fn remove(&mut self, h: Handle) -> bool {
        let slot = &mut self.slots[h.idx];
        if slot.generation == h.generation && slot.val.is_some() {
            slot.val = None;
            slot.generation += 1;
            self.free.push(h.idx);
            true
        } else {
            false
        }
    }
}

fn lcg(state: i64) -> i64 {
    (state * 48271) % 2147483647
}

fn main() {
    let mut reg = Registry::new();
    let mut handles: Vec<Handle> = Vec::new();
    let mut s: i64 = 20260813;
    let mut next_id: i64 = 0;

    let mut i: i64 = 0;
    while i < INITIAL {
        s = lcg(s);
        let h = reg.insert(Obj { id: next_id, hp: s % 100 + 1 });
        handles.push(h);
        next_id += 1;
        i += 1;
    }

    let mut chk: i64 = 0;
    let mut dead_deletes: i64 = 0;
    let mut r: i64 = 0;
    while r < ROUNDS {
        // Delete phase: victims drawn from every handle ever issued, so
        // some draws hit handles that are already stale.
        let mut d: i64 = 0;
        while d < DELETES {
            s = lcg(s);
            let j = (s % handles.len() as i64) as usize;
            if !reg.remove(handles[j]) {
                dead_deletes += 1;
            }
            d += 1;
        }
        // Spawn phase: reuses freed slots, bumping their generation.
        let mut a: i64 = 0;
        while a < SPAWNS {
            s = lcg(s);
            let h = reg.insert(Obj { id: next_id, hp: s % 100 + 1 });
            handles.push(h);
            next_id += 1;
            a += 1;
        }
        // Sweep phase: dereference EVERY handle ever issued.
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
