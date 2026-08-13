// resource_capabilities (std-only) — a device handle threaded through an
// ownership pipeline: charge (mutate-owned), transfer (hand off), audit
// (borrow). The safety property under test: after a value is moved, the
// old binding is DEAD — Rust's E0382 makes use-after-move a compile
// error (see rejected.rs), and Drop guarantees release exactly once.
//
// Output must be byte-identical to the Jestyr twin.

const N: i64 = 2_000_000;
const MOD: i64 = 1_000_000_007;

struct Device {
    id: i64,
    energy: i64,
}

fn lcg(state: i64) -> i64 {
    (state * 48271) % 2147483647
}

// Takes ownership, mutates, gives it back.
fn charge(mut d: Device, k: i64) -> Device {
    d.energy += k;
    d
}

// Pure handoff: the caller's binding dies, the callee's return re-owns.
fn transfer(d: Device) -> Device {
    d
}

// Borrow-only inspection.
fn audit(d: &Device) -> i64 {
    d.energy % 97 + d.id
}

fn main() {
    let mut d = Device { id: 7, energy: 0 };
    let mut s: i64 = 20260813;
    let mut acc: i64 = 0;
    let mut i: i64 = 0;
    while i < N {
        s = lcg(s);
        d = charge(d, s % 50);
        d = transfer(d);
        acc = (acc + audit(&d)) % MOD;
        i += 1;
    }
    println!("{}", acc);
    println!("{}", d.energy);
    println!("{}", d.id);
}
