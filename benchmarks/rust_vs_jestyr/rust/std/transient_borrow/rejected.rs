// REJECTION PROBE — must NOT compile.
// Reading the world through a & while an &mut is still live: the exact
// overlap the safe version proves impossible.

struct World {
    scores: Vec<i64>,
}

fn total(w: &World) -> i64 {
    let mut t = 0;
    for s in &w.scores {
        t += s;
    }
    t
}

fn main() {
    let mut w = World { scores: vec![1, 2, 3] };
    let cell = &mut w.scores[0];
    let t = total(&w); // ERROR: w is mutably borrowed by `cell`
    *cell += t;
    println!("{}", t);
}
