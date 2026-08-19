/* nbody, from the Computer Language Benchmarks Game.
 *
 * A transcription of examples/nbody.vyrn: same algorithm, same constants, same
 * order of floating-point operations. Build with:
 *
 *   clang -O2 -ffp-contract=off -std=c11 -o nbody.exe nbody.c
 *
 * N is argv[1] (the number of steps); without it the census N is used.
 */

#include <stdio.h>
#include <stdlib.h>
#include <math.h>

/* The census N -- 1000 steps, the size the committed fixture was written at. */
#define CENSUS_STEPS 1000

#define NBODIES 5

typedef struct {
    double x, y, z;
    double vx, vy, vz;
    double mass;
} Body;

/* The Vyrn program takes one lane of F64x2.sqrt, which is IEEE sqrt. */
static double sqrtF(double v)
{
    return sqrt(v);
}

/* `v` at nine decimal places, the format the game prints. The sign comes off
 * first, then the digits are scaled into a 64-bit integer and the fraction is
 * padded -- exactly the shape of the Vyrn `fixed9`. */
static void printFixed9(double v)
{
    const char *sign = "";
    double a = v;
    long long scaled;

    if (a < 0.0) {
        sign = "-";
        a = 0.0 - a;
    }
    scaled = (long long)(a * 1000000000.0 + 0.5);
    printf("%s%lld.%09lld\n", sign, scaled / 1000000000, scaled % 1000000000);
}

static double solarMass(void)
{
    return 4.0 * 3.141592653589793 * 3.141592653589793;
}

static double daysPerYear(void)
{
    return 365.24;
}

/* The sun and the four gas giants, at the positions and velocities the game
 * publishes. */
static void initSystem(Body *b)
{
    double sm = solarMass();
    double dy = daysPerYear();

    b[0].x = 0.0;
    b[0].y = 0.0;
    b[0].z = 0.0;
    b[0].vx = 0.0;
    b[0].vy = 0.0;
    b[0].vz = 0.0;
    b[0].mass = sm;

    b[1].x = 4.84143144246472090;
    b[1].y = -1.16032004402742839;
    b[1].z = -0.103622044471123109;
    b[1].vx = 0.00166007664274403694 * dy;
    b[1].vy = 0.00769901118419740425 * dy;
    b[1].vz = -0.0000690460016972063023 * dy;
    b[1].mass = 0.000954791938424326609 * sm;

    b[2].x = 8.34336671824457987;
    b[2].y = 4.12479856412430479;
    b[2].z = -0.403523417114321381;
    b[2].vx = -0.00276742510726862411 * dy;
    b[2].vy = 0.00499852801234917238 * dy;
    b[2].vz = 0.0000230417297573763929 * dy;
    b[2].mass = 0.000285885980666130812 * sm;

    b[3].x = 12.8943695621391310;
    b[3].y = -15.1111514016986312;
    b[3].z = -0.223307578892655734;
    b[3].vx = 0.00296460137564761618 * dy;
    b[3].vy = 0.00237847173959480950 * dy;
    b[3].vz = -0.0000296589568540237556 * dy;
    b[3].mass = 0.0000436624404335156298 * sm;

    b[4].x = 15.3796971148509165;
    b[4].y = -25.9193146099879641;
    b[4].z = 0.179258772950371181;
    b[4].vx = 0.00268067772490389322 * dy;
    b[4].vy = 0.00162824170038242295 * dy;
    b[4].vz = -0.0000951592254519715870 * dy;
    b[4].mass = 0.0000515138902046611451 * sm;
}

/* The sun's velocity, set so the whole system's momentum is zero. */
static void offsetMomentum(Body *b, int n)
{
    double px = 0.0;
    double py = 0.0;
    double pz = 0.0;
    int i;

    for (i = 0; i < n; i++) {
        px = px + b[i].vx * b[i].mass;
        py = py + b[i].vy * b[i].mass;
        pz = pz + b[i].vz * b[i].mass;
    }
    b[0].vx = 0.0 - px / solarMass();
    b[0].vy = 0.0 - py / solarMass();
    b[0].vz = 0.0 - pz / solarMass();
}

/* Kinetic energy plus the pairwise potential. */
static double energy(const Body *b, int n)
{
    double e = 0.0;
    int i, j;

    for (i = 0; i < n; i++) {
        e = e + 0.5 * b[i].mass * (b[i].vx * b[i].vx + b[i].vy * b[i].vy + b[i].vz * b[i].vz);
        for (j = i + 1; j < n; j++) {
            double dx = b[i].x - b[j].x;
            double dy = b[i].y - b[j].y;
            double dz = b[i].z - b[j].z;
            e = e - b[i].mass * b[j].mass / sqrtF(dx * dx + dy * dy + dz * dz);
        }
    }
    return e;
}

/* One time step: every pair exchanges momentum, then every body moves. */
static void advance(Body *b, int n, double dt)
{
    int i, j, k;

    for (i = 0; i < n; i++) {
        for (j = i + 1; j < n; j++) {
            double dx = b[i].x - b[j].x;
            double dy = b[i].y - b[j].y;
            double dz = b[i].z - b[j].z;
            double d2 = dx * dx + dy * dy + dz * dz;
            double mag = dt / (d2 * sqrtF(d2));
            double mi = b[i].mass;
            double mj = b[j].mass;
            b[i].vx = b[i].vx - dx * mj * mag;
            b[i].vy = b[i].vy - dy * mj * mag;
            b[i].vz = b[i].vz - dz * mj * mag;
            b[j].vx = b[j].vx + dx * mi * mag;
            b[j].vy = b[j].vy + dy * mi * mag;
            b[j].vz = b[j].vz + dz * mi * mag;
        }
    }
    for (k = 0; k < n; k++) {
        b[k].x = b[k].x + dt * b[k].vx;
        b[k].y = b[k].y + dt * b[k].vy;
        b[k].z = b[k].z + dt * b[k].vz;
    }
}

/* `n` steps from the published initial conditions, and the energy afterwards. */
static double integrate(long long n)
{
    Body b[NBODIES];
    long long i;

    initSystem(b);
    offsetMomentum(b, NBODIES);
    for (i = 0; i < n; i++) {
        advance(b, NBODIES, 0.01);
    }
    return energy(b, NBODIES);
}

int main(int argc, char **argv)
{
    long long steps = CENSUS_STEPS;
    Body b[NBODIES];

    if (argc > 1) {
        steps = strtoll(argv[1], NULL, 10);
    }

    initSystem(b);
    offsetMomentum(b, NBODIES);
    printFixed9(energy(b, NBODIES));
    printFixed9(integrate(steps));
    return 0;
}
