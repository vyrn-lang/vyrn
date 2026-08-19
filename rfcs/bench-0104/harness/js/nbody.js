// nbody, the JavaScript leg of RFC-0104 M2.
//
// The specification is examples/nbody.vyrn. Same algorithm, same constants, same
// floating-point expression order, byte-identical output.
//
//   $ node nbody.js            # the census N, 1000 steps
//   $ node nbody.js 2000000    # the bench N
//
// Where this differs structurally from the Vyrn program, it is because plain
// JavaScript has the thing Vyrn's std does not:
//
//   * Math.sqrt is the scalar square root Vyrn reaches through F64x2.sqrt(..).lane(0).
//   * advance and offsetMomentum mutate the array in place. Vyrn's ownership
//     model makes them `consume`-in / owned-out; JS has no such rule, so the
//     move-in-move-out shape simply is not written here.
//
// fixed9 is transcribed rather than replaced by toFixed, which rounds differently.

'use strict';
const fs = require('fs');

/// The census N — 1000 steps, the size the committed fixture was written at.
const steps = 1000;

let pending = '';
function emit(s) {
    pending += s;
    if (pending.length >= 1 << 20) flush();
}
function flush() {
    if (pending.length === 0) return;
    const buf = Buffer.from(pending, 'latin1');
    pending = '';
    let off = 0;
    while (off < buf.length) {
        try {
            off += fs.writeSync(1, buf, off, buf.length - off);
        } catch (e) {
            if (e.code !== 'EAGAIN') throw e;
        }
    }
}

/// `v` at nine decimal places, the format the game prints.
function fixed9(v) {
    let a = v;
    let sign = '';
    if (a < 0.0) {
        sign = '-';
        a = 0.0 - a;
    }
    const scaled = Math.trunc(a * 1000000000.0 + 0.5);
    const whole = Math.trunc(scaled / 1000000000);
    const frac = String(scaled % 1000000000).padStart(9, '0');
    return `${sign}${whole}.${frac}`;
}

function solarMass() {
    return 4.0 * 3.141592653589793 * 3.141592653589793;
}

function daysPerYear() {
    return 365.24;
}

/// The sun and the four gas giants, at the positions and velocities the game
/// publishes.
function system() {
    const sm = solarMass();
    const dy = daysPerYear();
    const b = [];
    b.push({ x: 0.0, y: 0.0, z: 0.0, vx: 0.0, vy: 0.0, vz: 0.0, mass: sm });
    b.push({
        x: 4.84143144246472090,
        y: -1.16032004402742839,
        z: -0.103622044471123109,
        vx: 0.00166007664274403694 * dy,
        vy: 0.00769901118419740425 * dy,
        vz: -0.0000690460016972063023 * dy,
        mass: 0.000954791938424326609 * sm,
    });
    b.push({
        x: 8.34336671824457987,
        y: 4.12479856412430479,
        z: -0.403523417114321381,
        vx: -0.00276742510726862411 * dy,
        vy: 0.00499852801234917238 * dy,
        vz: 0.0000230417297573763929 * dy,
        mass: 0.000285885980666130812 * sm,
    });
    b.push({
        x: 12.8943695621391310,
        y: -15.1111514016986312,
        z: -0.223307578892655734,
        vx: 0.00296460137564761618 * dy,
        vy: 0.00237847173959480950 * dy,
        vz: -0.0000296589568540237556 * dy,
        mass: 0.0000436624404335156298 * sm,
    });
    b.push({
        x: 15.3796971148509165,
        y: -25.9193146099879641,
        z: 0.179258772950371181,
        vx: 0.00268067772490389322 * dy,
        vy: 0.00162824170038242295 * dy,
        vz: -0.0000951592254519715870 * dy,
        mass: 0.0000515138902046611451 * sm,
    });
    return b;
}

/// The sun's velocity, set so the whole system's momentum is zero.
function offsetMomentum(b) {
    let px = 0.0;
    let py = 0.0;
    let pz = 0.0;
    for (const body of b) {
        px = px + body.vx * body.mass;
        py = py + body.vy * body.mass;
        pz = pz + body.vz * body.mass;
    }
    b[0].vx = 0.0 - px / solarMass();
    b[0].vy = 0.0 - py / solarMass();
    b[0].vz = 0.0 - pz / solarMass();
    return b;
}

/// Kinetic energy plus the pairwise potential.
function energy(b) {
    let e = 0.0;
    let i = 0;
    while (i < b.length) {
        e = e + 0.5 * b[i].mass * (b[i].vx * b[i].vx + b[i].vy * b[i].vy + b[i].vz * b[i].vz);
        let j = i + 1;
        while (j < b.length) {
            const dx = b[i].x - b[j].x;
            const dy = b[i].y - b[j].y;
            const dz = b[i].z - b[j].z;
            e = e - b[i].mass * b[j].mass / Math.sqrt(dx * dx + dy * dy + dz * dz);
            j = j + 1;
        }
        i = i + 1;
    }
    return e;
}

/// One time step: every pair exchanges momentum, then every body moves.
function advance(b, dt) {
    let i = 0;
    while (i < b.length) {
        let j = i + 1;
        while (j < b.length) {
            const dx = b[i].x - b[j].x;
            const dy = b[i].y - b[j].y;
            const dz = b[i].z - b[j].z;
            const d2 = dx * dx + dy * dy + dz * dz;
            const mag = dt / (d2 * Math.sqrt(d2));
            const mi = b[i].mass;
            const mj = b[j].mass;
            b[i].vx = b[i].vx - dx * mj * mag;
            b[i].vy = b[i].vy - dy * mj * mag;
            b[i].vz = b[i].vz - dz * mj * mag;
            b[j].vx = b[j].vx + dx * mi * mag;
            b[j].vy = b[j].vy + dy * mi * mag;
            b[j].vz = b[j].vz + dz * mi * mag;
            j = j + 1;
        }
        i = i + 1;
    }
    let k = 0;
    while (k < b.length) {
        b[k].x = b[k].x + dt * b[k].vx;
        b[k].y = b[k].y + dt * b[k].vy;
        b[k].z = b[k].z + dt * b[k].vz;
        k = k + 1;
    }
    return b;
}

/// `n` steps from the published initial conditions, and the energy afterwards.
function integrate(n) {
    let b = offsetMomentum(system());
    let i = 0;
    while (i < n) {
        b = advance(b, 0.01);
        i = i + 1;
    }
    return energy(b);
}

const n = process.argv[2] === undefined ? steps : Number(process.argv[2]);
emit(fixed9(energy(offsetMomentum(system()))) + '\n');
emit(fixed9(integrate(n)) + '\n');
flush();
