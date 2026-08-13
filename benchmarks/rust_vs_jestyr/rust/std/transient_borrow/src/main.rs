// transient_borrow — sum, advance, clamp, and inspect nested structs through
// short-lived borrows. The ownership property under test: passing a big
// aggregate to helpers immutably (&) and mutably (&mut) without copies,
// with the compiler proving no reader observes a half-updated world.
//
// Output must be byte-identical to the Jestyr twin.

const N: i64 = 1_000_000;
const TICKS: i64 = 40;

struct V3 {
    x: i64,
    y: i64,
    z: i64,
}

struct Player {
    pos: V3,
    vel: V3,
    hp: i64,
    score: i64,
}

struct World {
    players: Vec<Player>,
    tick: i64,
}

fn lcg(state: i64) -> i64 {
    (state * 48271) % 2147483647
}

fn make_world() -> World {
    let mut players = Vec::with_capacity(N as usize);
    let mut s: i64 = 20260813;
    let mut i: i64 = 0;
    while i < N {
        s = lcg(s);
        let px = s % 1000;
        s = lcg(s);
        let py = s % 1000;
        s = lcg(s);
        let pz = s % 1000;
        s = lcg(s);
        let vx = s % 9 - 4;
        s = lcg(s);
        let vy = s % 9 - 4;
        s = lcg(s);
        let vz = s % 9 - 4;
        s = lcg(s);
        let hp = s % 100 + 1;
        players.push(Player {
            pos: V3 { x: px, y: py, z: pz },
            vel: V3 { x: vx, y: vy, z: vz },
            hp,
            score: 0,
        });
        i += 1;
    }
    World { players, tick: 0 }
}

// Immutable borrow of the whole world: read every player, mutate nothing.
fn total_score(w: &World) -> i64 {
    let mut t: i64 = 0;
    for p in &w.players {
        t += p.score;
    }
    t
}

// Mutable borrow of one player: clamp hp into [0, 100].
fn clamp_hp(p: &mut Player) {
    if p.hp < 0 {
        p.hp = 0;
    }
    if p.hp > 100 {
        p.hp = 100;
    }
}

// Mutable borrow of the whole world: move everyone, damage on schedule,
// score the survivors.
fn advance(w: &mut World) {
    w.tick += 1;
    let damage_round = w.tick % 8 == 0;
    for p in &mut w.players {
        p.pos.x += p.vel.x;
        p.pos.y += p.vel.y;
        p.pos.z += p.vel.z;
        if damage_round {
            p.hp -= (p.pos.x + p.pos.y + p.pos.z) % 7;
            clamp_hp(p);
        }
        if p.hp > 0 {
            p.score += p.hp % 10;
        }
    }
}

// Immutable borrow again: fold the final state into one checksum.
fn inspect(w: &World) -> i64 {
    let mut c: i64 = 0;
    for p in &w.players {
        c += p.pos.x * 3 + p.pos.y * 5 + p.pos.z * 7 + p.hp * 11;
        c %= 1_000_000_007;
    }
    c
}

fn main() {
    let mut w = make_world();
    let mut score_trace: i64 = 0;
    let mut t: i64 = 0;
    while t < TICKS {
        advance(&mut w);
        score_trace = (score_trace + total_score(&w)) % 1_000_000_007;
        t += 1;
    }
    println!("{}", score_trace);
    println!("{}", inspect(&w));
    println!("{}", w.tick);
}
