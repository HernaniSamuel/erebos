/* C side of c_abi.ers: one function per row of the table of ADR 0025, and
 * one that calls Erebos functions with aggregates. */
#include <stdint.h>

struct One { uint8_t a; };
struct Three { uint8_t a, b, c; };
struct Word { uint32_t a; };
struct Pair { uint32_t a, b; };
struct Wide { uint64_t a; };
struct Big { uint32_t a, b, c; };
struct __attribute__((packed)) Packed { uint8_t a; uint32_t b; };
struct Bytes5 { uint8_t b[5]; }; /* the twin of [u8; 5] */
struct Huge { uint32_t w[16]; };

struct Pair erebos_swap(struct Pair p);
struct Big erebos_big(struct Big b, uint32_t k);
struct Three erebos_three(struct Three t);

struct One c_one(struct One x) { x.a += 1; return x; }

struct Three c_three(struct Three x) {
    struct Three r = { x.c, x.b, x.a };
    return r;
}

struct Word c_word(struct Word x) { x.a *= 2; return x; }

struct Pair c_pair(struct Pair x) {
    struct Pair r = { x.a + x.b, x.a - x.b };
    return r;
}

struct Wide c_wide(struct Wide x) { x.a *= 3; return x; }

/* Changes its copy; the caller's value must not change. */
struct Big c_big(struct Big x) {
    x.a += 1;
    x.b += 2;
    x.c += 3;
    return x;
}

struct Packed c_packed(struct Packed x) { x.b += x.a; return x; }

struct Bytes5 c_bytes(struct Bytes5 x) {
    for (int i = 0; i < 5; i++) {
        x.b[i] += 1;
    }
    return x;
}

uint32_t c_huge_sum(struct Huge h) {
    uint32_t sum = 0;
    for (int i = 0; i < 16; i++) {
        sum += h.w[i];
    }
    return sum;
}

/* Seven words take a0..a6: the pair gets a7 and the stack. */
struct Pair c_late(uint32_t a, uint32_t b, uint32_t c, uint32_t d, uint32_t e,
                   uint32_t f, uint32_t g, struct Pair x) {
    struct Pair r = { x.a + a + b + c + d, x.b + e + f + g };
    return r;
}

/* Declared in Erebos with a zero-sized parameter between the two. */
uint32_t c_around_empty(uint32_t a, uint32_t b) { return a * 10 + b; }

uint32_t c_calls_erebos(void) {
    struct Pair p = erebos_swap((struct Pair){ 1, 2 });
    struct Big b = erebos_big((struct Big){ 1, 2, 3 }, 10);
    struct Three t = erebos_three((struct Three){ 4, 5, 6 });
    return p.a * 100000 + p.b * 10000 + (b.a + b.b + b.c) * 100 + t.a * 10 + t.c;
}
