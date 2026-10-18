/* C side of enum_payloads_c.ers: the twin of an enum with payloads is a
 * union of structs that each start with the tag (ADR 0039), which clang
 * passes by its size and alignment, as Erebos does (ADR 0025). */
#include <stdint.h>

union One {
    uint8_t tag;
    struct { uint8_t tag; uint8_t p0; uint16_t p1; } B;
};

union Pair {
    uint8_t tag;
    struct { uint8_t tag; uint32_t p0; } B;
    struct { uint8_t tag; uint8_t p0; uint16_t p1; } C;
};

/* `B(())` has no C equivalent; its tag is all there is. */
union Long {
    uint64_t tag;
};

union Big {
    uint16_t tag;
    struct { uint16_t tag; uint32_t p0; uint32_t p1; } B;
    struct { uint16_t tag; uint8_t p0; } C;
};

union One erebos_one(union One x);
union Pair erebos_pair(union Pair x);
union Long erebos_long(union Long x);
union Big erebos_big(union Big x);

/* B(a, b) becomes B(a + 1, b * 2), and A becomes B(7, 7). */
union One c_one(union One x) {
    union One r;
    r.B.tag = 1;
    if (x.tag == 1) {
        r.B.p0 = x.B.p0 + 1;
        r.B.p1 = x.B.p1 * 2;
    } else {
        r.B.p0 = 7;
        r.B.p1 = 7;
    }
    return r;
}

/* B(v) becomes C(low byte of v, high half of v), C(a, b) becomes
 * B(a << 16 | b), and A stays A. */
union Pair c_pair(union Pair x) {
    union Pair r;
    if (x.tag == 1) {
        r.C.tag = 2;
        r.C.p0 = (uint8_t)x.B.p0;
        r.C.p1 = (uint16_t)(x.B.p0 >> 16);
    } else if (x.tag == 2) {
        r.B.tag = 1;
        r.B.p0 = (uint32_t)x.C.p0 << 16 | x.C.p1;
    } else {
        r.tag = 0;
    }
    return r;
}

/* A becomes B, and B becomes A. */
union Long c_long(union Long x) {
    union Long r;
    r.tag = x.tag ^ 1;
    return r;
}

/* B(a, b) becomes B(b, a), C(c) becomes C(c + 1), and A becomes C(0). */
union Big c_big(union Big x) {
    union Big r;
    if (x.tag == 1) {
        r.B.tag = 1;
        r.B.p0 = x.B.p1;
        r.B.p1 = x.B.p0;
    } else {
        r.C.tag = 2;
        r.C.p0 = x.tag == 2 ? x.C.p0 + 1 : 0;
    }
    return r;
}

/* Calls each Erebos function and sums the tags and payload values of what
 * they return: 1 + 40 + 301, 2 + 6 + 7, 1, 1 + 130 + 70. */
uint32_t c_calls_erebos(void) {
    union One one = { .B = { 1, 20, 300 } };
    one = erebos_one(one);
    union Pair pair = { .C = { 2, 5, 6 } };
    pair = erebos_pair(pair);
    union Long l = { .tag = 0 };
    l = erebos_long(l);
    union Big big = { .B = { 1, 100, 30 } };
    big = erebos_big(big);
    return one.tag + one.B.p0 + one.B.p1 + pair.tag + pair.C.p0 + pair.C.p1 + (uint32_t)l.tag
        + big.tag + big.B.p0 + big.B.p1;
}
