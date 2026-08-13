// structured_concurrency (std-only) — parallel sum of squares over
// 20,000,000 i64 with `std::thread::scope`: workers borrow the shared
// slice without 'static, the scope joins before the borrow ends. Fixed
// 4-way chunking (q = n/4, last chunk takes the remainder) and in-order
// merge, matching Jestyr's `core.par_reduce` grouping exactly. The
// result is checked against a serial pass in-program.
//
// NOTE the asymmetry this case exists to record: this program is
// deterministic because integer + is associative — swap the element to
// f64 and it silently is not, and nothing here would warn. Jestyr's
// `par for … reduce` REFUSES undeclared reductions at compile time.
//
// Output must be byte-identical to the idiomatic and Jestyr twins.

const N: usize = 20_000_000;
const MOD: i64 = 1_000_000_007;
const WORKERS: usize = 4;

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

    let q = N / WORKERS;
    let mut partials = [0i64; WORKERS];
    std::thread::scope(|scope| {
        let mut joins = Vec::with_capacity(WORKERS);
        for w in 0..WORKERS {
            let lo = w * q;
            let hi = if w == WORKERS - 1 { N } else { lo + q };
            let chunk = &xs[lo..hi];
            joins.push(scope.spawn(move || serial_sum_sq(chunk)));
        }
        for (w, j) in joins.into_iter().enumerate() {
            partials[w] = j.join().unwrap();
        }
    });
    let mut p: i64 = 0;
    for w in 0..WORKERS {
        p += partials[w];
    }

    println!("{}", p % MOD);
    println!("{}", if p == serial_sum_sq(&xs) { 1 } else { 0 });
}
