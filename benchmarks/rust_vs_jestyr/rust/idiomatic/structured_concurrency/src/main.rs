// structured_concurrency (idiomatic) — the rayon one-liner. Work-stealing
// schedule, any number of threads, and the sum is STILL deterministic —
// but only because i64 + is associative. rayon makes the same silent
// bet the std twin does; `.par_iter().map(..).sum()` over f64 would be
// schedule-dependent with no warning. That bet is the comparison.
//
// Output must be byte-identical to the std-only and Jestyr twins.

use rayon::prelude::*;

const N: usize = 20_000_000;
const MOD: i64 = 1_000_000_007;

fn serial_sum_sq(xs: &[i64]) -> i64 {
    let mut acc: i64 = 0;
    for &x in xs {
        acc += x * x;
    }
    acc
}

fn main() {
    let mut xs: Vec<i64> = Vec::with_capacity(N);
    let mut i: usize = 0;
    while i < N {
        xs.push((i as i64) % 1000);
        i += 1;
    }

    let p: i64 = xs.par_iter().map(|&x| x * x).sum();

    println!("{}", p % MOD);
    println!("{}", if p == serial_sum_sq(&xs) { 1 } else { 0 });
}
