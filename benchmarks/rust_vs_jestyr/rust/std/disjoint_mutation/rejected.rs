// REJECTION PROBE — must NOT compile.
// Two &mut into one buffer without reifying disjointness: even though the
// index ranges never overlap, Rust's aliasing rule refuses the shape —
// which is exactly why `split_at_mut` exists.

fn bump(half: &mut [i64]) {
    for v in half.iter_mut() {
        *v += 1;
    }
}

fn main() {
    let mut xs = vec![0i64; 16];
    let left = &mut xs[..8];
    let right = &mut xs[8..]; // ERROR: second mutable borrow of `xs`
    bump(left);
    bump(right);
    println!("{}", xs[0] + xs[15]);
}
