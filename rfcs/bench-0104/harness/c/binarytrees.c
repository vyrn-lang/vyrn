/* binary-trees, from the Computer Language Benchmarks Game.
 *
 * A transcription of examples/binarytrees.vyrn. Build with:
 *
 *   clang -O2 -ffp-contract=off -std=c11 -o binarytrees.exe binarytrees.c
 *
 * N is argv[1] (the maximum depth); without it the census N is used.
 *
 * The Vyrn program's `Tree` is `| Leaf | Node(Tree, Tree)`, and its release
 * happens on its own. Here a Leaf is a null pointer, a Node is a malloc'd pair,
 * and the release is a hand-written walk -- the benchmark measures the
 * allocator, so the trees really are allocated and really are freed.
 */

#include <stdio.h>
#include <stdlib.h>

/* The census N -- depth 10. */
#define CENSUS_ORDER 10

typedef struct Node {
    struct Node *left;
    struct Node *right;
} Node;

/* A complete tree of `depth`. */
static Node *make(long long depth)
{
    Node *n;

    if (depth == 0) {
        return NULL;
    }
    n = (Node *)malloc(sizeof(Node));
    if (n == NULL) {
        fprintf(stderr, "out of memory\n");
        exit(1);
    }
    n->left = make(depth - 1);
    n->right = make(depth - 1);
    return n;
}

/* The node count -- the game's checksum. A Leaf counts as 1. */
static long long check(const Node *t)
{
    if (t == NULL) {
        return 1;
    }
    return 1 + check(t->left) + check(t->right);
}

static void release(Node *t)
{
    if (t == NULL) {
        return;
    }
    release(t->left);
    release(t->right);
    free(t);
}

/* `iterations` trees of `depth`, built, checked and released one at a time. */
static long long checkAll(long long depth, long long iterations)
{
    long long sum = 0;
    long long i;

    for (i = 0; i < iterations; i++) {
        Node *t = make(depth);
        sum = sum + check(t);
        release(t);
    }
    return sum;
}

/* How many trees of `depth` the game asks for at this `maxDepth`. */
static long long iterationsFor(long long depth, long long maxDepth, long long minDepth)
{
    long long n = 1;
    long long s = 0;

    while (s < maxDepth - depth + minDepth) {
        n = n * 2;
        s = s + 1;
    }
    return n;
}

/* The whole run at `n`, printing as it goes. */
static void run(long long n)
{
    long long minDepth = 4;
    long long maxDepth = n;
    long long stretchDepth;
    long long depth;
    Node *stretch;
    Node *longLived;

    if (maxDepth < minDepth + 2) {
        maxDepth = minDepth + 2;
    }
    stretchDepth = maxDepth + 1;

    stretch = make(stretchDepth);
    printf("stretch tree of depth %lld\t check: %lld\n", stretchDepth, check(stretch));
    release(stretch);

    longLived = make(maxDepth);

    for (depth = minDepth; depth < stretchDepth; depth = depth + 2) {
        long long iterations = iterationsFor(depth, maxDepth, minDepth);
        long long sum = checkAll(depth, iterations);
        printf("%lld\t trees of depth %lld\t check: %lld\n", iterations, depth, sum);
    }
    printf("long lived tree of depth %lld\t check: %lld\n", maxDepth, check(longLived));
    release(longLived);
}

int main(int argc, char **argv)
{
    long long order = CENSUS_ORDER;

    if (argc > 1) {
        order = strtoll(argv[1], NULL, 10);
    }
    run(order);
    return 0;
}
