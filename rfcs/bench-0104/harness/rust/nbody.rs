// nbody, from the Computer Language Benchmarks Game.
//
// The Rust leg of RFC-0104 M2. It computes what `examples/nbody.vyrn` computes,
// in the same expression order, and prints the same bytes.
//
//   $ rustc -C opt-level=3 -o nbody.exe nbody.rs
//   $ ./nbody 1000
//
// Safe Rust, std only.

use std::env;

/// The census N — 1000 steps, the size the committed fixture was written at.
const STEPS: i64 = 1000;

#[derive(Clone, Copy)]
struct Body {
    x: f64,
    y: f64,
    z: f64,
    vx: f64,
    vy: f64,
    vz: f64,
    mass: f64,
}

/// `v` at nine decimal places, the format the game prints.
fn fixed9(v: f64) -> String {
    let mut a = v;
    let mut sign = "";
    if a < 0.0 {
        sign = "-";
        a = 0.0 - a;
    }
    let scaled = (a * 1000000000.0 + 0.5) as i64;
    format!("{}{}.{:09}", sign, scaled / 1000000000, scaled % 1000000000)
}

fn solar_mass() -> f64 {
    4.0 * 3.141592653589793 * 3.141592653589793
}

fn days_per_year() -> f64 {
    365.24
}

/// The sun and the four gas giants, at the positions and velocities the game
/// publishes.
fn system() -> [Body; 5] {
    let sm = solar_mass();
    let dy = days_per_year();
    [
        Body { x: 0.0, y: 0.0, z: 0.0, vx: 0.0, vy: 0.0, vz: 0.0, mass: sm },
        Body {
            x: 4.84143144246472090,
            y: -1.16032004402742839,
            z: -0.103622044471123109,
            vx: 0.00166007664274403694 * dy,
            vy: 0.00769901118419740425 * dy,
            vz: -0.0000690460016972063023 * dy,
            mass: 0.000954791938424326609 * sm,
        },
        Body {
            x: 8.34336671824457987,
            y: 4.12479856412430479,
            z: -0.403523417114321381,
            vx: -0.00276742510726862411 * dy,
            vy: 0.00499852801234917238 * dy,
            vz: 0.0000230417297573763929 * dy,
            mass: 0.000285885980666130812 * sm,
        },
        Body {
            x: 12.8943695621391310,
            y: -15.1111514016986312,
            z: -0.223307578892655734,
            vx: 0.00296460137564761618 * dy,
            vy: 0.00237847173959480950 * dy,
            vz: -0.0000296589568540237556 * dy,
            mass: 0.0000436624404335156298 * sm,
        },
        Body {
            x: 15.3796971148509165,
            y: -25.9193146099879641,
            z: 0.179258772950371181,
            vx: 0.00268067772490389322 * dy,
            vy: 0.00162824170038242295 * dy,
            vz: -0.0000951592254519715870 * dy,
            mass: 0.0000515138902046611451 * sm,
        },
    ]
}

/// The sun's velocity, set so the whole system's momentum is zero.
fn offset_momentum(b: &mut [Body]) {
    let mut px = 0.0;
    let mut py = 0.0;
    let mut pz = 0.0;
    for body in b.iter() {
        px = px + body.vx * body.mass;
        py = py + body.vy * body.mass;
        pz = pz + body.vz * body.mass;
    }
    b[0].vx = 0.0 - px / solar_mass();
    b[0].vy = 0.0 - py / solar_mass();
    b[0].vz = 0.0 - pz / solar_mass();
}

/// Kinetic energy plus the pairwise potential.
fn energy(b: &[Body]) -> f64 {
    let mut e = 0.0;
    for i in 0..b.len() {
        e = e + 0.5 * b[i].mass * (b[i].vx * b[i].vx + b[i].vy * b[i].vy + b[i].vz * b[i].vz);
        for j in i + 1..b.len() {
            let dx = b[i].x - b[j].x;
            let dy = b[i].y - b[j].y;
            let dz = b[i].z - b[j].z;
            e = e - b[i].mass * b[j].mass / (dx * dx + dy * dy + dz * dz).sqrt();
        }
    }
    e
}

/// One time step: every pair exchanges momentum, then every body moves.
fn advance(b: &mut [Body], dt: f64) {
    for i in 0..b.len() {
        for j in i + 1..b.len() {
            let dx = b[i].x - b[j].x;
            let dy = b[i].y - b[j].y;
            let dz = b[i].z - b[j].z;
            let d2 = dx * dx + dy * dy + dz * dz;
            let mag = dt / (d2 * d2.sqrt());
            let mi = b[i].mass;
            let mj = b[j].mass;
            b[i].vx = b[i].vx - dx * mj * mag;
            b[i].vy = b[i].vy - dy * mj * mag;
            b[i].vz = b[i].vz - dz * mj * mag;
            b[j].vx = b[j].vx + dx * mi * mag;
            b[j].vy = b[j].vy + dy * mi * mag;
            b[j].vz = b[j].vz + dz * mi * mag;
        }
    }
    for k in 0..b.len() {
        b[k].x = b[k].x + dt * b[k].vx;
        b[k].y = b[k].y + dt * b[k].vy;
        b[k].z = b[k].z + dt * b[k].vz;
    }
}

/// `n` steps from the published initial conditions, and the energy afterwards.
fn integrate(n: i64) -> f64 {
    let mut b = system();
    offset_momentum(&mut b);
    for _ in 0..n {
        advance(&mut b, 0.01);
    }
    energy(&b)
}

fn main() {
    let n: i64 = env::args()
        .nth(1)
        .map(|a| a.parse().expect("N must be an integer"))
        .unwrap_or(STEPS);

    let mut b = system();
    offset_momentum(&mut b);
    println!("{}", fixed9(energy(&b)));
    println!("{}", fixed9(integrate(n)));
}
