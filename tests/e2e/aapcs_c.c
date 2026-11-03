/* C side of aapcs_c.ers: one function per case in each direction, with the
 * same patterns as the Erebos side. */
#include <stdint.h>
#include <stddef.h>

/* Byte i of the pattern seed. */
static void fill(void *p, size_t n, uint8_t seed) {
    uint8_t *bytes = p;
    for (size_t i = 0; i < n; i++) {
        bytes[i] = (uint8_t)(seed + i * 7 + 1);
    }
}

static int check(const void *p, size_t n, uint8_t seed) {
    const uint8_t *bytes = p;
    for (size_t i = 0; i < n; i++) {
        if (bytes[i] != (uint8_t)(seed + i * 7 + 1)) {
            return 0;
        }
    }
    return 1;
}


/* 1 byte: `[1 x i32]`, returned as `i8` */
struct B1 { uint8_t b[1]; };

/* 2 bytes: returned as `i16` */
struct B2 { uint8_t b[2]; };

/* 3 bytes: returned as `i32` */
struct B3 { uint8_t b[3]; };

/* 4 bytes, alignment 1: returned as `i32` */
struct B4 { uint8_t b[4]; };

/* 5 bytes, alignment 1: `[2 x i32]`, returned through `sret` */
struct B5 { uint8_t b[5]; };

/* one word */
struct W4 { uint32_t w; };

/* two words: `[2 x i32]` */
struct X8 { uint32_t w[2]; };

/* three words: `[3 x i32]` */
struct X12 { uint32_t w[3]; };

/* `{ uint64_t }`: `[1 x i64]` */
struct L8 { uint64_t a[1]; };

/* two `u64`: `[2 x i64]` */
struct L16 { uint64_t a[2]; };

/* three `u64`: `[3 x i64]` */
struct L24 { uint64_t a[3]; };

/* 64 bytes, alignment 8: `[8 x i64]` */
struct L64 { uint64_t a[8]; };

/* 64 bytes, alignment 4: `[16 x i32]` */
struct X64 { uint32_t w[16]; };

/* 68 bytes: `byval align 4` */
struct X68 { uint32_t w[17]; };

/* 72 bytes, alignment 8: `byval align 8` */
struct X72 { uint64_t a[9]; };

/* `#[align(16)]` struct of words: `[4 x i32]` */
struct __attribute__((aligned(16))) A16 { uint32_t w[4]; };

/* 80 bytes, `#[align(16)]`: `byval align 4`, copied to an aligned slot */
struct __attribute__((aligned(16))) A16Big { uint32_t w[20]; };

/* holds an `#[align(16)]` struct: `[2 x i64]` */
struct Nest16 { struct A16 a; };

/* holds two: `[4 x i64]` */
struct Nest32 { struct A16 a; struct A16 b; };

/* 9 bytes, `#[packed]`: `[3 x i32]` */
struct __attribute__((packed)) P9 { uint8_t a; uint32_t w[2]; };

/* an array of three words, wrapped in a struct in C */
struct Arr12 { uint32_t w[3]; };

/* The Erebos side declares a zero-sized parameter between the two. */
uint32_t c_empty(uint32_t a, uint32_t b) { return a * 10 + b; }
uint32_t e_empty(uint32_t a, uint32_t b);
int c_calls_empty(void) { return e_empty(3, 4) == 34; }

/* A function returning a zero-sized struct returns nothing: no hidden pointer
 * takes `r0`, so the arguments stay where they are. The C twin returns void. */
uint32_t empty_seen_by_c;
extern uint32_t empty_seen_by_erebos;
void c_empty_ret(uint32_t a, uint32_t b) { empty_seen_by_c = a * 10 + b; }
void e_empty_ret(uint32_t a, uint32_t b);
int c_calls_empty_ret(void) {
    empty_seen_by_erebos = 0;
    e_empty_ret(5, 6);
    return empty_seen_by_erebos == 56;
}

struct B1 e_b1_0(struct B1 x, uint32_t z);
struct B2 e_b2_0(struct B2 x, uint32_t z);
struct B3 e_b3_0(struct B3 x, uint32_t z);
struct B4 e_b4_0(struct B4 x, uint32_t z);
struct B5 e_b5_0(struct B5 x, uint32_t z);
struct W4 e_w4_0(struct W4 x, uint32_t z);
struct X8 e_x8_0(struct X8 x, uint32_t z);
struct L8 e_l8_0(struct L8 x, uint32_t z);
struct L16 e_l16_0(struct L16 x, uint32_t z);
struct L24 e_l24_0(struct L24 x, uint32_t z);
struct L64 e_l64_0(struct L64 x, uint32_t z);
struct X64 e_x64_0(struct X64 x, uint32_t z);
struct X68 e_x68_0(struct X68 x, uint32_t z);
struct X72 e_x72_0(struct X72 x, uint32_t z);
struct A16 e_a16_0(struct A16 x, uint32_t z);
struct A16Big e_a16big_0(struct A16Big x, uint32_t z);
struct Nest16 e_nest16_0(struct Nest16 x, uint32_t z);
struct Nest32 e_nest32_0(struct Nest32 x, uint32_t z);
struct P9 e_p9_0(struct P9 x, uint32_t z);
struct Arr12 e_arr12_0(struct Arr12 x, uint32_t z);
struct L8 e_l8_1(uint32_t a0, struct L8 x, uint32_t z);
struct L16 e_l16_1(uint32_t a0, struct L16 x, uint32_t z);
struct L24 e_l24_1(uint32_t a0, struct L24 x, uint32_t z);
struct L64 e_l64_1(uint32_t a0, struct L64 x, uint32_t z);
struct X8 e_x8_1(uint32_t a0, struct X8 x, uint32_t z);
struct X68 e_x68_1(uint32_t a0, struct X68 x, uint32_t z);
struct X72 e_x72_1(uint32_t a0, struct X72 x, uint32_t z);
struct A16 e_a16_1(uint32_t a0, struct A16 x, uint32_t z);
struct A16Big e_a16big_1(uint32_t a0, struct A16Big x, uint32_t z);
struct Nest16 e_nest16_1(uint32_t a0, struct Nest16 x, uint32_t z);
struct Nest32 e_nest32_1(uint32_t a0, struct Nest32 x, uint32_t z);
struct X8 e_x8_2(uint32_t a0, uint32_t a1, struct X8 x, uint32_t z);
struct B5 e_b5_2(uint32_t a0, uint32_t a1, struct B5 x, uint32_t z);
struct A16 e_a16_2(uint32_t a0, uint32_t a1, struct A16 x, uint32_t z);
struct X64 e_x64_2(uint32_t a0, uint32_t a1, struct X64 x, uint32_t z);
struct X12 e_x12_2(uint32_t a0, uint32_t a1, struct X12 x, uint32_t z);
struct P9 e_p9_2(uint32_t a0, uint32_t a1, struct P9 x, uint32_t z);
struct L8 e_l8_2(uint32_t a0, uint32_t a1, struct L8 x, uint32_t z);
struct X68 e_x68_2(uint32_t a0, uint32_t a1, struct X68 x, uint32_t z);
struct B5 e_b5_3(uint32_t a0, uint32_t a1, uint32_t a2, struct B5 x, uint32_t z);
struct X8 e_x8_3(uint32_t a0, uint32_t a1, uint32_t a2, struct X8 x, uint32_t z);
struct A16 e_a16_3(uint32_t a0, uint32_t a1, uint32_t a2, struct A16 x, uint32_t z);
struct L8 e_l8_3(uint32_t a0, uint32_t a1, uint32_t a2, struct L8 x, uint32_t z);
struct L16 e_l16_3(uint32_t a0, uint32_t a1, uint32_t a2, struct L16 x, uint32_t z);
struct Nest16 e_nest16_3(uint32_t a0, uint32_t a1, uint32_t a2, struct Nest16 x, uint32_t z);
struct X68 e_x68_3(uint32_t a0, uint32_t a1, uint32_t a2, struct X68 x, uint32_t z);
struct X72 e_x72_3(uint32_t a0, uint32_t a1, uint32_t a2, struct X72 x, uint32_t z);
struct X64 e_x64_3(uint32_t a0, uint32_t a1, uint32_t a2, struct X64 x, uint32_t z);
struct B3 e_b3_4(uint32_t a0, uint32_t a1, uint32_t a2, uint32_t a3, struct B3 x, uint32_t z);
struct X8 e_x8_4(uint32_t a0, uint32_t a1, uint32_t a2, uint32_t a3, struct X8 x, uint32_t z);
struct X68 e_x68_4(uint32_t a0, uint32_t a1, uint32_t a2, uint32_t a3, struct X68 x, uint32_t z);
struct L16 e_l16_4(uint32_t a0, uint32_t a1, uint32_t a2, uint32_t a3, struct L16 x, uint32_t z);
struct A16Big e_a16big_4(uint32_t a0, uint32_t a1, uint32_t a2, uint32_t a3, struct A16Big x, uint32_t z);

struct B1 c_b1_0(struct B1 x, uint32_t z) {
    struct B1 r;
    int ok = z == 99 && check(&x, sizeof x, 1)
        && (uintptr_t)&x % _Alignof(struct B1) == 0;
    fill(&r, sizeof r, ok ? 2 : 3);
    fill(&x, sizeof x, 77);
    return r;
}
int c_calls_b1_0(void) {
    struct B1 x;
    fill(&x, sizeof x, 1);
    struct B1 r = e_b1_0(x, 99);
    return check(&r, sizeof r, 2) && check(&x, sizeof x, 1);
}

struct B2 c_b2_0(struct B2 x, uint32_t z) {
    struct B2 r;
    int ok = z == 99 && check(&x, sizeof x, 1)
        && (uintptr_t)&x % _Alignof(struct B2) == 0;
    fill(&r, sizeof r, ok ? 2 : 3);
    fill(&x, sizeof x, 77);
    return r;
}
int c_calls_b2_0(void) {
    struct B2 x;
    fill(&x, sizeof x, 1);
    struct B2 r = e_b2_0(x, 99);
    return check(&r, sizeof r, 2) && check(&x, sizeof x, 1);
}

struct B3 c_b3_0(struct B3 x, uint32_t z) {
    struct B3 r;
    int ok = z == 99 && check(&x, sizeof x, 1)
        && (uintptr_t)&x % _Alignof(struct B3) == 0;
    fill(&r, sizeof r, ok ? 2 : 3);
    fill(&x, sizeof x, 77);
    return r;
}
int c_calls_b3_0(void) {
    struct B3 x;
    fill(&x, sizeof x, 1);
    struct B3 r = e_b3_0(x, 99);
    return check(&r, sizeof r, 2) && check(&x, sizeof x, 1);
}

struct B4 c_b4_0(struct B4 x, uint32_t z) {
    struct B4 r;
    int ok = z == 99 && check(&x, sizeof x, 1)
        && (uintptr_t)&x % _Alignof(struct B4) == 0;
    fill(&r, sizeof r, ok ? 2 : 3);
    fill(&x, sizeof x, 77);
    return r;
}
int c_calls_b4_0(void) {
    struct B4 x;
    fill(&x, sizeof x, 1);
    struct B4 r = e_b4_0(x, 99);
    return check(&r, sizeof r, 2) && check(&x, sizeof x, 1);
}

struct B5 c_b5_0(struct B5 x, uint32_t z) {
    struct B5 r;
    int ok = z == 99 && check(&x, sizeof x, 1)
        && (uintptr_t)&x % _Alignof(struct B5) == 0;
    fill(&r, sizeof r, ok ? 2 : 3);
    fill(&x, sizeof x, 77);
    return r;
}
int c_calls_b5_0(void) {
    struct B5 x;
    fill(&x, sizeof x, 1);
    struct B5 r = e_b5_0(x, 99);
    return check(&r, sizeof r, 2) && check(&x, sizeof x, 1);
}

struct W4 c_w4_0(struct W4 x, uint32_t z) {
    struct W4 r;
    int ok = z == 99 && check(&x, sizeof x, 1)
        && (uintptr_t)&x % _Alignof(struct W4) == 0;
    fill(&r, sizeof r, ok ? 2 : 3);
    fill(&x, sizeof x, 77);
    return r;
}
int c_calls_w4_0(void) {
    struct W4 x;
    fill(&x, sizeof x, 1);
    struct W4 r = e_w4_0(x, 99);
    return check(&r, sizeof r, 2) && check(&x, sizeof x, 1);
}

struct X8 c_x8_0(struct X8 x, uint32_t z) {
    struct X8 r;
    int ok = z == 99 && check(&x, sizeof x, 1)
        && (uintptr_t)&x % _Alignof(struct X8) == 0;
    fill(&r, sizeof r, ok ? 2 : 3);
    fill(&x, sizeof x, 77);
    return r;
}
int c_calls_x8_0(void) {
    struct X8 x;
    fill(&x, sizeof x, 1);
    struct X8 r = e_x8_0(x, 99);
    return check(&r, sizeof r, 2) && check(&x, sizeof x, 1);
}

struct L8 c_l8_0(struct L8 x, uint32_t z) {
    struct L8 r;
    int ok = z == 99 && check(&x, sizeof x, 1)
        && (uintptr_t)&x % _Alignof(struct L8) == 0;
    fill(&r, sizeof r, ok ? 2 : 3);
    fill(&x, sizeof x, 77);
    return r;
}
int c_calls_l8_0(void) {
    struct L8 x;
    fill(&x, sizeof x, 1);
    struct L8 r = e_l8_0(x, 99);
    return check(&r, sizeof r, 2) && check(&x, sizeof x, 1);
}

struct L16 c_l16_0(struct L16 x, uint32_t z) {
    struct L16 r;
    int ok = z == 99 && check(&x, sizeof x, 1)
        && (uintptr_t)&x % _Alignof(struct L16) == 0;
    fill(&r, sizeof r, ok ? 2 : 3);
    fill(&x, sizeof x, 77);
    return r;
}
int c_calls_l16_0(void) {
    struct L16 x;
    fill(&x, sizeof x, 1);
    struct L16 r = e_l16_0(x, 99);
    return check(&r, sizeof r, 2) && check(&x, sizeof x, 1);
}

struct L24 c_l24_0(struct L24 x, uint32_t z) {
    struct L24 r;
    int ok = z == 99 && check(&x, sizeof x, 1)
        && (uintptr_t)&x % _Alignof(struct L24) == 0;
    fill(&r, sizeof r, ok ? 2 : 3);
    fill(&x, sizeof x, 77);
    return r;
}
int c_calls_l24_0(void) {
    struct L24 x;
    fill(&x, sizeof x, 1);
    struct L24 r = e_l24_0(x, 99);
    return check(&r, sizeof r, 2) && check(&x, sizeof x, 1);
}

struct L64 c_l64_0(struct L64 x, uint32_t z) {
    struct L64 r;
    int ok = z == 99 && check(&x, sizeof x, 1)
        && (uintptr_t)&x % _Alignof(struct L64) == 0;
    fill(&r, sizeof r, ok ? 2 : 3);
    fill(&x, sizeof x, 77);
    return r;
}
int c_calls_l64_0(void) {
    struct L64 x;
    fill(&x, sizeof x, 1);
    struct L64 r = e_l64_0(x, 99);
    return check(&r, sizeof r, 2) && check(&x, sizeof x, 1);
}

struct X64 c_x64_0(struct X64 x, uint32_t z) {
    struct X64 r;
    int ok = z == 99 && check(&x, sizeof x, 1)
        && (uintptr_t)&x % _Alignof(struct X64) == 0;
    fill(&r, sizeof r, ok ? 2 : 3);
    fill(&x, sizeof x, 77);
    return r;
}
int c_calls_x64_0(void) {
    struct X64 x;
    fill(&x, sizeof x, 1);
    struct X64 r = e_x64_0(x, 99);
    return check(&r, sizeof r, 2) && check(&x, sizeof x, 1);
}

struct X68 c_x68_0(struct X68 x, uint32_t z) {
    struct X68 r;
    int ok = z == 99 && check(&x, sizeof x, 1)
        && (uintptr_t)&x % _Alignof(struct X68) == 0;
    fill(&r, sizeof r, ok ? 2 : 3);
    fill(&x, sizeof x, 77);
    return r;
}
int c_calls_x68_0(void) {
    struct X68 x;
    fill(&x, sizeof x, 1);
    struct X68 r = e_x68_0(x, 99);
    return check(&r, sizeof r, 2) && check(&x, sizeof x, 1);
}

struct X72 c_x72_0(struct X72 x, uint32_t z) {
    struct X72 r;
    int ok = z == 99 && check(&x, sizeof x, 1)
        && (uintptr_t)&x % _Alignof(struct X72) == 0;
    fill(&r, sizeof r, ok ? 2 : 3);
    fill(&x, sizeof x, 77);
    return r;
}
int c_calls_x72_0(void) {
    struct X72 x;
    fill(&x, sizeof x, 1);
    struct X72 r = e_x72_0(x, 99);
    return check(&r, sizeof r, 2) && check(&x, sizeof x, 1);
}

struct A16 c_a16_0(struct A16 x, uint32_t z) {
    struct A16 r;
    int ok = z == 99 && check(&x, sizeof x, 1)
        && (uintptr_t)&x % _Alignof(struct A16) == 0;
    fill(&r, sizeof r, ok ? 2 : 3);
    fill(&x, sizeof x, 77);
    return r;
}
int c_calls_a16_0(void) {
    struct A16 x;
    fill(&x, sizeof x, 1);
    struct A16 r = e_a16_0(x, 99);
    return check(&r, sizeof r, 2) && check(&x, sizeof x, 1);
}

struct A16Big c_a16big_0(struct A16Big x, uint32_t z) {
    struct A16Big r;
    int ok = z == 99 && check(&x, sizeof x, 1);
    fill(&r, sizeof r, ok ? 2 : 3);
    fill(&x, sizeof x, 77);
    return r;
}
int c_calls_a16big_0(void) {
    struct A16Big x;
    fill(&x, sizeof x, 1);
    struct A16Big r = e_a16big_0(x, 99);
    return check(&r, sizeof r, 2) && check(&x, sizeof x, 1);
}

struct Nest16 c_nest16_0(struct Nest16 x, uint32_t z) {
    struct Nest16 r;
    int ok = z == 99 && check(&x, sizeof x, 1)
        && (uintptr_t)&x % _Alignof(struct Nest16) == 0;
    fill(&r, sizeof r, ok ? 2 : 3);
    fill(&x, sizeof x, 77);
    return r;
}
int c_calls_nest16_0(void) {
    struct Nest16 x;
    fill(&x, sizeof x, 1);
    struct Nest16 r = e_nest16_0(x, 99);
    return check(&r, sizeof r, 2) && check(&x, sizeof x, 1);
}

struct Nest32 c_nest32_0(struct Nest32 x, uint32_t z) {
    struct Nest32 r;
    int ok = z == 99 && check(&x, sizeof x, 1)
        && (uintptr_t)&x % _Alignof(struct Nest32) == 0;
    fill(&r, sizeof r, ok ? 2 : 3);
    fill(&x, sizeof x, 77);
    return r;
}
int c_calls_nest32_0(void) {
    struct Nest32 x;
    fill(&x, sizeof x, 1);
    struct Nest32 r = e_nest32_0(x, 99);
    return check(&r, sizeof r, 2) && check(&x, sizeof x, 1);
}

struct P9 c_p9_0(struct P9 x, uint32_t z) {
    struct P9 r;
    int ok = z == 99 && check(&x, sizeof x, 1)
        && (uintptr_t)&x % _Alignof(struct P9) == 0;
    fill(&r, sizeof r, ok ? 2 : 3);
    fill(&x, sizeof x, 77);
    return r;
}
int c_calls_p9_0(void) {
    struct P9 x;
    fill(&x, sizeof x, 1);
    struct P9 r = e_p9_0(x, 99);
    return check(&r, sizeof r, 2) && check(&x, sizeof x, 1);
}

struct Arr12 c_arr12_0(struct Arr12 x, uint32_t z) {
    struct Arr12 r;
    int ok = z == 99 && check(&x, sizeof x, 1)
        && (uintptr_t)&x % _Alignof(struct Arr12) == 0;
    fill(&r, sizeof r, ok ? 2 : 3);
    fill(&x, sizeof x, 77);
    return r;
}
int c_calls_arr12_0(void) {
    struct Arr12 x;
    fill(&x, sizeof x, 1);
    struct Arr12 r = e_arr12_0(x, 99);
    return check(&r, sizeof r, 2) && check(&x, sizeof x, 1);
}

struct L8 c_l8_1(uint32_t a0, struct L8 x, uint32_t z) {
    struct L8 r;
    int ok = a0 == 10 && z == 99 && check(&x, sizeof x, 1)
        && (uintptr_t)&x % _Alignof(struct L8) == 0;
    fill(&r, sizeof r, ok ? 2 : 3);
    fill(&x, sizeof x, 77);
    return r;
}
int c_calls_l8_1(void) {
    struct L8 x;
    fill(&x, sizeof x, 1);
    struct L8 r = e_l8_1(10, x, 99);
    return check(&r, sizeof r, 2) && check(&x, sizeof x, 1);
}

struct L16 c_l16_1(uint32_t a0, struct L16 x, uint32_t z) {
    struct L16 r;
    int ok = a0 == 10 && z == 99 && check(&x, sizeof x, 1)
        && (uintptr_t)&x % _Alignof(struct L16) == 0;
    fill(&r, sizeof r, ok ? 2 : 3);
    fill(&x, sizeof x, 77);
    return r;
}
int c_calls_l16_1(void) {
    struct L16 x;
    fill(&x, sizeof x, 1);
    struct L16 r = e_l16_1(10, x, 99);
    return check(&r, sizeof r, 2) && check(&x, sizeof x, 1);
}

struct L24 c_l24_1(uint32_t a0, struct L24 x, uint32_t z) {
    struct L24 r;
    int ok = a0 == 10 && z == 99 && check(&x, sizeof x, 1)
        && (uintptr_t)&x % _Alignof(struct L24) == 0;
    fill(&r, sizeof r, ok ? 2 : 3);
    fill(&x, sizeof x, 77);
    return r;
}
int c_calls_l24_1(void) {
    struct L24 x;
    fill(&x, sizeof x, 1);
    struct L24 r = e_l24_1(10, x, 99);
    return check(&r, sizeof r, 2) && check(&x, sizeof x, 1);
}

struct L64 c_l64_1(uint32_t a0, struct L64 x, uint32_t z) {
    struct L64 r;
    int ok = a0 == 10 && z == 99 && check(&x, sizeof x, 1)
        && (uintptr_t)&x % _Alignof(struct L64) == 0;
    fill(&r, sizeof r, ok ? 2 : 3);
    fill(&x, sizeof x, 77);
    return r;
}
int c_calls_l64_1(void) {
    struct L64 x;
    fill(&x, sizeof x, 1);
    struct L64 r = e_l64_1(10, x, 99);
    return check(&r, sizeof r, 2) && check(&x, sizeof x, 1);
}

struct X8 c_x8_1(uint32_t a0, struct X8 x, uint32_t z) {
    struct X8 r;
    int ok = a0 == 10 && z == 99 && check(&x, sizeof x, 1)
        && (uintptr_t)&x % _Alignof(struct X8) == 0;
    fill(&r, sizeof r, ok ? 2 : 3);
    fill(&x, sizeof x, 77);
    return r;
}
int c_calls_x8_1(void) {
    struct X8 x;
    fill(&x, sizeof x, 1);
    struct X8 r = e_x8_1(10, x, 99);
    return check(&r, sizeof r, 2) && check(&x, sizeof x, 1);
}

struct X68 c_x68_1(uint32_t a0, struct X68 x, uint32_t z) {
    struct X68 r;
    int ok = a0 == 10 && z == 99 && check(&x, sizeof x, 1)
        && (uintptr_t)&x % _Alignof(struct X68) == 0;
    fill(&r, sizeof r, ok ? 2 : 3);
    fill(&x, sizeof x, 77);
    return r;
}
int c_calls_x68_1(void) {
    struct X68 x;
    fill(&x, sizeof x, 1);
    struct X68 r = e_x68_1(10, x, 99);
    return check(&r, sizeof r, 2) && check(&x, sizeof x, 1);
}

struct X72 c_x72_1(uint32_t a0, struct X72 x, uint32_t z) {
    struct X72 r;
    int ok = a0 == 10 && z == 99 && check(&x, sizeof x, 1)
        && (uintptr_t)&x % _Alignof(struct X72) == 0;
    fill(&r, sizeof r, ok ? 2 : 3);
    fill(&x, sizeof x, 77);
    return r;
}
int c_calls_x72_1(void) {
    struct X72 x;
    fill(&x, sizeof x, 1);
    struct X72 r = e_x72_1(10, x, 99);
    return check(&r, sizeof r, 2) && check(&x, sizeof x, 1);
}

struct A16 c_a16_1(uint32_t a0, struct A16 x, uint32_t z) {
    struct A16 r;
    int ok = a0 == 10 && z == 99 && check(&x, sizeof x, 1)
        && (uintptr_t)&x % _Alignof(struct A16) == 0;
    fill(&r, sizeof r, ok ? 2 : 3);
    fill(&x, sizeof x, 77);
    return r;
}
int c_calls_a16_1(void) {
    struct A16 x;
    fill(&x, sizeof x, 1);
    struct A16 r = e_a16_1(10, x, 99);
    return check(&r, sizeof r, 2) && check(&x, sizeof x, 1);
}

struct A16Big c_a16big_1(uint32_t a0, struct A16Big x, uint32_t z) {
    struct A16Big r;
    int ok = a0 == 10 && z == 99 && check(&x, sizeof x, 1);
    fill(&r, sizeof r, ok ? 2 : 3);
    fill(&x, sizeof x, 77);
    return r;
}
int c_calls_a16big_1(void) {
    struct A16Big x;
    fill(&x, sizeof x, 1);
    struct A16Big r = e_a16big_1(10, x, 99);
    return check(&r, sizeof r, 2) && check(&x, sizeof x, 1);
}

struct Nest16 c_nest16_1(uint32_t a0, struct Nest16 x, uint32_t z) {
    struct Nest16 r;
    int ok = a0 == 10 && z == 99 && check(&x, sizeof x, 1)
        && (uintptr_t)&x % _Alignof(struct Nest16) == 0;
    fill(&r, sizeof r, ok ? 2 : 3);
    fill(&x, sizeof x, 77);
    return r;
}
int c_calls_nest16_1(void) {
    struct Nest16 x;
    fill(&x, sizeof x, 1);
    struct Nest16 r = e_nest16_1(10, x, 99);
    return check(&r, sizeof r, 2) && check(&x, sizeof x, 1);
}

struct Nest32 c_nest32_1(uint32_t a0, struct Nest32 x, uint32_t z) {
    struct Nest32 r;
    int ok = a0 == 10 && z == 99 && check(&x, sizeof x, 1)
        && (uintptr_t)&x % _Alignof(struct Nest32) == 0;
    fill(&r, sizeof r, ok ? 2 : 3);
    fill(&x, sizeof x, 77);
    return r;
}
int c_calls_nest32_1(void) {
    struct Nest32 x;
    fill(&x, sizeof x, 1);
    struct Nest32 r = e_nest32_1(10, x, 99);
    return check(&r, sizeof r, 2) && check(&x, sizeof x, 1);
}

struct X8 c_x8_2(uint32_t a0, uint32_t a1, struct X8 x, uint32_t z) {
    struct X8 r;
    int ok = a0 == 10 && a1 == 11 && z == 99 && check(&x, sizeof x, 1)
        && (uintptr_t)&x % _Alignof(struct X8) == 0;
    fill(&r, sizeof r, ok ? 2 : 3);
    fill(&x, sizeof x, 77);
    return r;
}
int c_calls_x8_2(void) {
    struct X8 x;
    fill(&x, sizeof x, 1);
    struct X8 r = e_x8_2(10, 11, x, 99);
    return check(&r, sizeof r, 2) && check(&x, sizeof x, 1);
}

struct B5 c_b5_2(uint32_t a0, uint32_t a1, struct B5 x, uint32_t z) {
    struct B5 r;
    int ok = a0 == 10 && a1 == 11 && z == 99 && check(&x, sizeof x, 1)
        && (uintptr_t)&x % _Alignof(struct B5) == 0;
    fill(&r, sizeof r, ok ? 2 : 3);
    fill(&x, sizeof x, 77);
    return r;
}
int c_calls_b5_2(void) {
    struct B5 x;
    fill(&x, sizeof x, 1);
    struct B5 r = e_b5_2(10, 11, x, 99);
    return check(&r, sizeof r, 2) && check(&x, sizeof x, 1);
}

struct A16 c_a16_2(uint32_t a0, uint32_t a1, struct A16 x, uint32_t z) {
    struct A16 r;
    int ok = a0 == 10 && a1 == 11 && z == 99 && check(&x, sizeof x, 1)
        && (uintptr_t)&x % _Alignof(struct A16) == 0;
    fill(&r, sizeof r, ok ? 2 : 3);
    fill(&x, sizeof x, 77);
    return r;
}
int c_calls_a16_2(void) {
    struct A16 x;
    fill(&x, sizeof x, 1);
    struct A16 r = e_a16_2(10, 11, x, 99);
    return check(&r, sizeof r, 2) && check(&x, sizeof x, 1);
}

struct X64 c_x64_2(uint32_t a0, uint32_t a1, struct X64 x, uint32_t z) {
    struct X64 r;
    int ok = a0 == 10 && a1 == 11 && z == 99 && check(&x, sizeof x, 1)
        && (uintptr_t)&x % _Alignof(struct X64) == 0;
    fill(&r, sizeof r, ok ? 2 : 3);
    fill(&x, sizeof x, 77);
    return r;
}
int c_calls_x64_2(void) {
    struct X64 x;
    fill(&x, sizeof x, 1);
    struct X64 r = e_x64_2(10, 11, x, 99);
    return check(&r, sizeof r, 2) && check(&x, sizeof x, 1);
}

struct X12 c_x12_2(uint32_t a0, uint32_t a1, struct X12 x, uint32_t z) {
    struct X12 r;
    int ok = a0 == 10 && a1 == 11 && z == 99 && check(&x, sizeof x, 1)
        && (uintptr_t)&x % _Alignof(struct X12) == 0;
    fill(&r, sizeof r, ok ? 2 : 3);
    fill(&x, sizeof x, 77);
    return r;
}
int c_calls_x12_2(void) {
    struct X12 x;
    fill(&x, sizeof x, 1);
    struct X12 r = e_x12_2(10, 11, x, 99);
    return check(&r, sizeof r, 2) && check(&x, sizeof x, 1);
}

struct P9 c_p9_2(uint32_t a0, uint32_t a1, struct P9 x, uint32_t z) {
    struct P9 r;
    int ok = a0 == 10 && a1 == 11 && z == 99 && check(&x, sizeof x, 1)
        && (uintptr_t)&x % _Alignof(struct P9) == 0;
    fill(&r, sizeof r, ok ? 2 : 3);
    fill(&x, sizeof x, 77);
    return r;
}
int c_calls_p9_2(void) {
    struct P9 x;
    fill(&x, sizeof x, 1);
    struct P9 r = e_p9_2(10, 11, x, 99);
    return check(&r, sizeof r, 2) && check(&x, sizeof x, 1);
}

struct L8 c_l8_2(uint32_t a0, uint32_t a1, struct L8 x, uint32_t z) {
    struct L8 r;
    int ok = a0 == 10 && a1 == 11 && z == 99 && check(&x, sizeof x, 1)
        && (uintptr_t)&x % _Alignof(struct L8) == 0;
    fill(&r, sizeof r, ok ? 2 : 3);
    fill(&x, sizeof x, 77);
    return r;
}
int c_calls_l8_2(void) {
    struct L8 x;
    fill(&x, sizeof x, 1);
    struct L8 r = e_l8_2(10, 11, x, 99);
    return check(&r, sizeof r, 2) && check(&x, sizeof x, 1);
}

struct X68 c_x68_2(uint32_t a0, uint32_t a1, struct X68 x, uint32_t z) {
    struct X68 r;
    int ok = a0 == 10 && a1 == 11 && z == 99 && check(&x, sizeof x, 1)
        && (uintptr_t)&x % _Alignof(struct X68) == 0;
    fill(&r, sizeof r, ok ? 2 : 3);
    fill(&x, sizeof x, 77);
    return r;
}
int c_calls_x68_2(void) {
    struct X68 x;
    fill(&x, sizeof x, 1);
    struct X68 r = e_x68_2(10, 11, x, 99);
    return check(&r, sizeof r, 2) && check(&x, sizeof x, 1);
}

struct B5 c_b5_3(uint32_t a0, uint32_t a1, uint32_t a2, struct B5 x, uint32_t z) {
    struct B5 r;
    int ok = a0 == 10 && a1 == 11 && a2 == 12 && z == 99 && check(&x, sizeof x, 1)
        && (uintptr_t)&x % _Alignof(struct B5) == 0;
    fill(&r, sizeof r, ok ? 2 : 3);
    fill(&x, sizeof x, 77);
    return r;
}
int c_calls_b5_3(void) {
    struct B5 x;
    fill(&x, sizeof x, 1);
    struct B5 r = e_b5_3(10, 11, 12, x, 99);
    return check(&r, sizeof r, 2) && check(&x, sizeof x, 1);
}

struct X8 c_x8_3(uint32_t a0, uint32_t a1, uint32_t a2, struct X8 x, uint32_t z) {
    struct X8 r;
    int ok = a0 == 10 && a1 == 11 && a2 == 12 && z == 99 && check(&x, sizeof x, 1)
        && (uintptr_t)&x % _Alignof(struct X8) == 0;
    fill(&r, sizeof r, ok ? 2 : 3);
    fill(&x, sizeof x, 77);
    return r;
}
int c_calls_x8_3(void) {
    struct X8 x;
    fill(&x, sizeof x, 1);
    struct X8 r = e_x8_3(10, 11, 12, x, 99);
    return check(&r, sizeof r, 2) && check(&x, sizeof x, 1);
}

struct A16 c_a16_3(uint32_t a0, uint32_t a1, uint32_t a2, struct A16 x, uint32_t z) {
    struct A16 r;
    int ok = a0 == 10 && a1 == 11 && a2 == 12 && z == 99 && check(&x, sizeof x, 1)
        && (uintptr_t)&x % _Alignof(struct A16) == 0;
    fill(&r, sizeof r, ok ? 2 : 3);
    fill(&x, sizeof x, 77);
    return r;
}
int c_calls_a16_3(void) {
    struct A16 x;
    fill(&x, sizeof x, 1);
    struct A16 r = e_a16_3(10, 11, 12, x, 99);
    return check(&r, sizeof r, 2) && check(&x, sizeof x, 1);
}

struct L8 c_l8_3(uint32_t a0, uint32_t a1, uint32_t a2, struct L8 x, uint32_t z) {
    struct L8 r;
    int ok = a0 == 10 && a1 == 11 && a2 == 12 && z == 99 && check(&x, sizeof x, 1)
        && (uintptr_t)&x % _Alignof(struct L8) == 0;
    fill(&r, sizeof r, ok ? 2 : 3);
    fill(&x, sizeof x, 77);
    return r;
}
int c_calls_l8_3(void) {
    struct L8 x;
    fill(&x, sizeof x, 1);
    struct L8 r = e_l8_3(10, 11, 12, x, 99);
    return check(&r, sizeof r, 2) && check(&x, sizeof x, 1);
}

struct L16 c_l16_3(uint32_t a0, uint32_t a1, uint32_t a2, struct L16 x, uint32_t z) {
    struct L16 r;
    int ok = a0 == 10 && a1 == 11 && a2 == 12 && z == 99 && check(&x, sizeof x, 1)
        && (uintptr_t)&x % _Alignof(struct L16) == 0;
    fill(&r, sizeof r, ok ? 2 : 3);
    fill(&x, sizeof x, 77);
    return r;
}
int c_calls_l16_3(void) {
    struct L16 x;
    fill(&x, sizeof x, 1);
    struct L16 r = e_l16_3(10, 11, 12, x, 99);
    return check(&r, sizeof r, 2) && check(&x, sizeof x, 1);
}

struct Nest16 c_nest16_3(uint32_t a0, uint32_t a1, uint32_t a2, struct Nest16 x, uint32_t z) {
    struct Nest16 r;
    int ok = a0 == 10 && a1 == 11 && a2 == 12 && z == 99 && check(&x, sizeof x, 1)
        && (uintptr_t)&x % _Alignof(struct Nest16) == 0;
    fill(&r, sizeof r, ok ? 2 : 3);
    fill(&x, sizeof x, 77);
    return r;
}
int c_calls_nest16_3(void) {
    struct Nest16 x;
    fill(&x, sizeof x, 1);
    struct Nest16 r = e_nest16_3(10, 11, 12, x, 99);
    return check(&r, sizeof r, 2) && check(&x, sizeof x, 1);
}

struct X68 c_x68_3(uint32_t a0, uint32_t a1, uint32_t a2, struct X68 x, uint32_t z) {
    struct X68 r;
    int ok = a0 == 10 && a1 == 11 && a2 == 12 && z == 99 && check(&x, sizeof x, 1)
        && (uintptr_t)&x % _Alignof(struct X68) == 0;
    fill(&r, sizeof r, ok ? 2 : 3);
    fill(&x, sizeof x, 77);
    return r;
}
int c_calls_x68_3(void) {
    struct X68 x;
    fill(&x, sizeof x, 1);
    struct X68 r = e_x68_3(10, 11, 12, x, 99);
    return check(&r, sizeof r, 2) && check(&x, sizeof x, 1);
}

struct X72 c_x72_3(uint32_t a0, uint32_t a1, uint32_t a2, struct X72 x, uint32_t z) {
    struct X72 r;
    int ok = a0 == 10 && a1 == 11 && a2 == 12 && z == 99 && check(&x, sizeof x, 1)
        && (uintptr_t)&x % _Alignof(struct X72) == 0;
    fill(&r, sizeof r, ok ? 2 : 3);
    fill(&x, sizeof x, 77);
    return r;
}
int c_calls_x72_3(void) {
    struct X72 x;
    fill(&x, sizeof x, 1);
    struct X72 r = e_x72_3(10, 11, 12, x, 99);
    return check(&r, sizeof r, 2) && check(&x, sizeof x, 1);
}

struct X64 c_x64_3(uint32_t a0, uint32_t a1, uint32_t a2, struct X64 x, uint32_t z) {
    struct X64 r;
    int ok = a0 == 10 && a1 == 11 && a2 == 12 && z == 99 && check(&x, sizeof x, 1)
        && (uintptr_t)&x % _Alignof(struct X64) == 0;
    fill(&r, sizeof r, ok ? 2 : 3);
    fill(&x, sizeof x, 77);
    return r;
}
int c_calls_x64_3(void) {
    struct X64 x;
    fill(&x, sizeof x, 1);
    struct X64 r = e_x64_3(10, 11, 12, x, 99);
    return check(&r, sizeof r, 2) && check(&x, sizeof x, 1);
}

struct B3 c_b3_4(uint32_t a0, uint32_t a1, uint32_t a2, uint32_t a3, struct B3 x, uint32_t z) {
    struct B3 r;
    int ok = a0 == 10 && a1 == 11 && a2 == 12 && a3 == 13 && z == 99 && check(&x, sizeof x, 1)
        && (uintptr_t)&x % _Alignof(struct B3) == 0;
    fill(&r, sizeof r, ok ? 2 : 3);
    fill(&x, sizeof x, 77);
    return r;
}
int c_calls_b3_4(void) {
    struct B3 x;
    fill(&x, sizeof x, 1);
    struct B3 r = e_b3_4(10, 11, 12, 13, x, 99);
    return check(&r, sizeof r, 2) && check(&x, sizeof x, 1);
}

struct X8 c_x8_4(uint32_t a0, uint32_t a1, uint32_t a2, uint32_t a3, struct X8 x, uint32_t z) {
    struct X8 r;
    int ok = a0 == 10 && a1 == 11 && a2 == 12 && a3 == 13 && z == 99 && check(&x, sizeof x, 1)
        && (uintptr_t)&x % _Alignof(struct X8) == 0;
    fill(&r, sizeof r, ok ? 2 : 3);
    fill(&x, sizeof x, 77);
    return r;
}
int c_calls_x8_4(void) {
    struct X8 x;
    fill(&x, sizeof x, 1);
    struct X8 r = e_x8_4(10, 11, 12, 13, x, 99);
    return check(&r, sizeof r, 2) && check(&x, sizeof x, 1);
}

struct X68 c_x68_4(uint32_t a0, uint32_t a1, uint32_t a2, uint32_t a3, struct X68 x, uint32_t z) {
    struct X68 r;
    int ok = a0 == 10 && a1 == 11 && a2 == 12 && a3 == 13 && z == 99 && check(&x, sizeof x, 1)
        && (uintptr_t)&x % _Alignof(struct X68) == 0;
    fill(&r, sizeof r, ok ? 2 : 3);
    fill(&x, sizeof x, 77);
    return r;
}
int c_calls_x68_4(void) {
    struct X68 x;
    fill(&x, sizeof x, 1);
    struct X68 r = e_x68_4(10, 11, 12, 13, x, 99);
    return check(&r, sizeof r, 2) && check(&x, sizeof x, 1);
}

struct L16 c_l16_4(uint32_t a0, uint32_t a1, uint32_t a2, uint32_t a3, struct L16 x, uint32_t z) {
    struct L16 r;
    int ok = a0 == 10 && a1 == 11 && a2 == 12 && a3 == 13 && z == 99 && check(&x, sizeof x, 1)
        && (uintptr_t)&x % _Alignof(struct L16) == 0;
    fill(&r, sizeof r, ok ? 2 : 3);
    fill(&x, sizeof x, 77);
    return r;
}
int c_calls_l16_4(void) {
    struct L16 x;
    fill(&x, sizeof x, 1);
    struct L16 r = e_l16_4(10, 11, 12, 13, x, 99);
    return check(&r, sizeof r, 2) && check(&x, sizeof x, 1);
}

struct A16Big c_a16big_4(uint32_t a0, uint32_t a1, uint32_t a2, uint32_t a3, struct A16Big x, uint32_t z) {
    struct A16Big r;
    int ok = a0 == 10 && a1 == 11 && a2 == 12 && a3 == 13 && z == 99 && check(&x, sizeof x, 1);
    fill(&r, sizeof r, ok ? 2 : 3);
    fill(&x, sizeof x, 77);
    return r;
}
int c_calls_a16big_4(void) {
    struct A16Big x;
    fill(&x, sizeof x, 1);
    struct A16Big r = e_a16big_4(10, 11, 12, 13, x, 99);
    return check(&r, sizeof r, 2) && check(&x, sizeof x, 1);
}

