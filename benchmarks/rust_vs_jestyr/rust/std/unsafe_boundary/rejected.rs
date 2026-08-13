// REJECTION PROBE — must NOT compile.
// The raw pointer op WITHOUT its unsafe fence: Rust refuses the deref in
// a safe context (E0133). The fence is compiler-required, not a comment
// convention.

fn poke(p: *mut i64, v: i64) {
    *p = v; // ERROR: dereference of raw pointer requires unsafe
}

fn main() {
    let mut x: i64 = 0;
    poke(&mut x as *mut i64, 42);
    println!("{}", x);
}
