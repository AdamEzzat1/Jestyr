// unsafe_boundary (std-only) — a SIMULATED register block (heap memory
// standing in for MMIO; no real hardware) wrapped in a safe API. The
// property under test: how small and well-fenced the unsafe kernel of a
// safe abstraction is. Exactly two unsafe blocks, each one raw pointer
// op, each preceded by the bounds assert that justifies it. Safe callers
// cannot reach the pointer.
//
// Output must be byte-identical to the Jestyr twin.

const REGS: usize = 64;
const OPS: i64 = 10_000_000;
const MOD: i64 = 1_000_000_007;

struct RegBlock {
    base: *mut i64,
    len: usize,
}

impl RegBlock {
    // SAFETY invariant: base points at len contiguous i64, owned by the
    // backing buffer that outlives this block; i < len is checked here.
    fn write(&mut self, i: usize, v: i64) {
        assert!(i < self.len);
        unsafe {
            *self.base.add(i) = v;
        }
    }

    fn read(&self, i: usize) -> i64 {
        assert!(i < self.len);
        unsafe { *self.base.add(i) }
    }
}

fn lcg(state: i64) -> i64 {
    (state * 48271) % 2147483647
}

fn main() {
    let mut backing = vec![0i64; REGS];
    let mut rb = RegBlock { base: backing.as_mut_ptr(), len: REGS };

    let mut s: i64 = 20260813;
    let mut acc: i64 = 0;
    let mut i: i64 = 0;
    while i < OPS {
        s = lcg(s);
        let idx = (s % REGS as i64) as usize;
        s = lcg(s);
        if s % 3 == 0 {
            rb.write(idx, s % 1000);
        } else {
            acc = (acc + rb.read(idx)) % MOD;
        }
        i += 1;
    }

    let mut total: i64 = 0;
    let mut r: usize = 0;
    while r < REGS {
        total += rb.read(r);
        r += 1;
    }
    println!("{}", acc);
    println!("{}", total);
}
