// disjoint_mutation — mutate two non-overlapping halves of one buffer.
// The ownership property under test: Rust forbids two &mut into one slice,
// so disjointness must be reified through an API the standard library
// blesses (`split_at_mut`, internally unsafe, externally safe). The
// interesting comparison is what a language WITHOUT the two-&mut ban
// needs to express the same program.
//
// Output must be byte-identical to the Jestyr twin.

const N: i64 = 8_000_000;
const ROUNDS: i64 = 25;

fn lcg(state: i64) -> i64 {
    (state * 48271) % 2147483647
}

fn make_buf() -> Vec<i64> {
    let mut xs = Vec::with_capacity(N as usize);
    let mut s: i64 = 20260813;
    let mut i: i64 = 0;
    while i < N {
        s = lcg(s);
        xs.push(s % 1000);
        i += 1;
    }
    xs
}

// Each helper takes its own exclusive slice — the signatures prove the
// two mutations cannot alias.
fn bump(half: &mut [i64]) {
    let mut i: i64 = 0;
    let n = half.len() as i64;
    while i < n {
        half[i as usize] += i % 7;
        i += 1;
    }
}

fn scale(half: &mut [i64]) {
    let mut i: i64 = 0;
    let n = half.len() as i64;
    while i < n {
        half[i as usize] = (half[i as usize] * 3 + i) % 100_003;
        i += 1;
    }
}

// One writer, one reader, same buffer, provably disjoint.
fn add_into(dst: &mut [i64], src: &[i64]) {
    let n = dst.len().min(src.len()) as i64;
    let mut i: i64 = 0;
    while i < n {
        dst[i as usize] = (dst[i as usize] + src[i as usize]) % 100_003;
        i += 1;
    }
}

fn checksum(xs: &[i64]) -> i64 {
    let mut c: i64 = 0;
    for &v in xs {
        c = (c * 31 + v) % 1_000_000_007;
    }
    c
}

fn main() {
    let mut xs = make_buf();
    let mid = (N / 2) as usize;
    let mut r: i64 = 0;
    while r < ROUNDS {
        let (left, right) = xs.split_at_mut(mid);
        bump(left);
        scale(right);
        add_into(left, right);
        r += 1;
    }
    println!("{}", checksum(&xs));
    println!("{}", xs[0]);
    println!("{}", xs[mid]);
    println!("{}", xs[(N - 1) as usize]);
}
