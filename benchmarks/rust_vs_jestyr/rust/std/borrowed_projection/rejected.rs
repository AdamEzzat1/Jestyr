// REJECTION PROBE — must NOT compile.
// A projection outliving the storage it points into: the returned borrow
// pins the vector, so dropping/moving the vector while the borrow lives
// is refused.

struct Token {
    kind: i64,
    len: i64,
}

fn first(xs: &[Token]) -> &Token {
    &xs[0]
}

fn main() {
    let f;
    {
        let toks = vec![Token { kind: 1, len: 3 }];
        f = first(&toks);
    } // ERROR: `toks` dropped while still borrowed by `f`
    println!("{}", f.kind + f.len);
}
