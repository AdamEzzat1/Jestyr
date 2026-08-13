// REJECTION PROBE — must NOT compile.
// Use after move: the capability was handed to `consume`, so the old
// binding is dead. This is the guarantee a linear-ish resource story
// stands on.

struct Device {
    id: i64,
    energy: i64,
}

fn consume(d: Device) -> i64 {
    d.energy + d.id
}

fn main() {
    let d = Device { id: 1, energy: 10 };
    let e = consume(d);
    println!("{}", d.energy); // ERROR: value borrowed here after move
    println!("{}", e);
}
