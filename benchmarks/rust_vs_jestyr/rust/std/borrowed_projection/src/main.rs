// borrowed_projection — functions that return a borrow INTO their argument:
// first element, indexed element, max element, a projected field. The
// ownership property under test: a callee can hand back a reference into
// caller-owned storage, and the borrow checker pins the container for as
// long as the reference lives. No copy, no index round-trip.
//
// Output must be byte-identical to the Jestyr twin.

const N: i64 = 2_000_000;
const LOOKUPS: i64 = 2_000_000;

struct Token {
    kind: i64,
    start: i64,
    len: i64,
}

fn lcg(state: i64) -> i64 {
    (state * 48271) % 2147483647
}

fn make_tokens() -> Vec<Token> {
    let mut toks = Vec::with_capacity(N as usize);
    let mut s: i64 = 20260813;
    let mut start: i64 = 0;
    let mut i: i64 = 0;
    while i < N {
        s = lcg(s);
        let kind = s % 12;
        s = lcg(s);
        let len = s % 24 + 1;
        toks.push(Token { kind, start, len });
        start += len;
        i += 1;
    }
    toks
}

// A borrow into the slice: the classic projection Rust writes natively.
fn first(xs: &[Token]) -> &Token {
    &xs[0]
}

fn at(xs: &[Token], i: i64) -> &Token {
    &xs[i as usize]
}

// Max-by-len, first occurrence wins. Returns a borrow, not a copy.
fn longest(xs: &[Token]) -> &Token {
    let mut best = &xs[0];
    for t in xs {
        if t.len > best.len {
            best = t;
        }
    }
    best
}

// Field projection through a borrowed struct.
fn kind_of(t: &Token) -> i64 {
    t.kind
}

fn main() {
    let toks = make_tokens();

    // Random-access lookups through the borrowed return of `at`.
    let mut s: i64 = 777;
    let mut sum: i64 = 0;
    let mut i: i64 = 0;
    while i < LOOKUPS {
        s = lcg(s);
        let t = at(&toks, s % N);
        sum = (sum + t.start % 1_000_003 + t.len * 31 + kind_of(t)) % 1_000_000_007;
        i += 1;
    }

    // Whole-slice projections.
    let f = first(&toks);
    let l = longest(&toks);

    println!("{}", sum);
    println!("{}", f.kind * 1000 + f.len);
    println!("{}", l.start % 1_000_003);
    println!("{}", l.len);
}
