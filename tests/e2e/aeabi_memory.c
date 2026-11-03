/* C side of aeabi_memory.ers: calls the twelve functions of the ARM EABI
 * that `core` defines (ADR 0074), which nothing calls on riscv32 but C code
 * naming them. Each check is numbered in the order it runs; `c_check_aeabi`
 * returns the number of the first that failed, or 0. */
#include <stddef.h>
#include <stdint.h>

void __aeabi_memcpy(void *dest, const void *src, size_t n);
void __aeabi_memcpy4(void *dest, const void *src, size_t n);
void __aeabi_memcpy8(void *dest, const void *src, size_t n);
void __aeabi_memmove(void *dest, const void *src, size_t n);
void __aeabi_memmove4(void *dest, const void *src, size_t n);
void __aeabi_memmove8(void *dest, const void *src, size_t n);
/* The length comes before the value, unlike `memset`'s. */
void __aeabi_memset(void *dest, size_t n, int c);
void __aeabi_memset4(void *dest, size_t n, int c);
void __aeabi_memset8(void *dest, size_t n, int c);
void __aeabi_memclr(void *dest, size_t n);
void __aeabi_memclr4(void *dest, size_t n);
void __aeabi_memclr8(void *dest, size_t n);

typedef void (*copy_fn)(void *, const void *, size_t);
typedef void (*set_fn)(void *, size_t, int);
typedef void (*clr_fn)(void *, size_t);

/* Larger than any length below, so that a swapped `n` and `c` stays inside. */
#define SIZE 512
#define GUARD 0xEE

static _Alignas(8) uint8_t area[SIZE];
static _Alignas(8) uint8_t other[SIZE];

static uint32_t checked;
static uint32_t failed;

static void check(int ok) {
    checked++;
    if (!ok && failed == 0) {
        failed = checked;
    }
}

/* Byte `i` of a pattern: never GUARD, and never the byte `check_set` stores,
 * for `i` below 64. */
static uint8_t pattern(size_t i) { return (uint8_t)(i + 1); }

static void fill_pattern(uint8_t *buf, size_t n) {
    for (size_t i = 0; i < n; i++) {
        buf[i] = pattern(i);
    }
}

static void fill_guard(uint8_t *buf, size_t n) {
    for (size_t i = 0; i < n; i++) {
        buf[i] = GUARD;
    }
}

/* Whether `buf[0..n]` holds `value`. */
static int all_equal(const uint8_t *buf, size_t n, uint8_t value) {
    for (size_t i = 0; i < n; i++) {
        if (buf[i] != value) {
            return 0;
        }
    }
    return 1;
}

/* Copies 20 bytes from `other + src_off` to `area + dest_off` (areas that do
 * not overlap), checks them and the bytes around them, and then copies none. */
static void check_copy(copy_fn f, size_t dest_off, size_t src_off) {
    fill_pattern(other, 64);
    fill_guard(area, 64);
    f(area + dest_off, other + src_off, 20);
    int same = 1;
    for (size_t i = 0; i < 20; i++) {
        same = same && area[dest_off + i] == pattern(src_off + i);
    }
    check(same);
    check(all_equal(area, dest_off, GUARD));
    check(all_equal(area + dest_off + 20, 64 - dest_off - 20, GUARD));

    fill_guard(area, 64);
    f(area + dest_off, other + src_off, 0);
    check(all_equal(area, 64, GUARD));
}

/* Moves 24 bytes of a pattern `shift` bytes up, which overlaps, and then the
 * same bytes `shift` bytes down, and then none. */
static void check_move(copy_fn f, size_t shift) {
    fill_pattern(area, 64);
    f(area + 8 + shift, area + 8, 24);
    int same = 1;
    for (size_t i = 0; i < 24; i++) {
        same = same && area[8 + shift + i] == pattern(8 + i);
    }
    check(same);
    check(area[7] == pattern(7) && area[8] == pattern(8));
    check(area[8 + shift + 24] == pattern(8 + shift + 24));

    fill_pattern(area, 64);
    f(area + 8, area + 8 + shift, 24);
    same = 1;
    for (size_t i = 0; i < 24; i++) {
        same = same && area[8 + i] == pattern(8 + shift + i);
    }
    check(same);
    check(area[7] == pattern(7));
    check(area[8 + 24] == pattern(8 + 24));

    fill_pattern(area, 64);
    f(area + 8, area + 8 + shift, 0);
    int unchanged = 1;
    for (size_t i = 0; i < 64; i++) {
        unchanged = unchanged && area[i] == pattern(i);
    }
    check(unchanged);
}

/* Fills 9 bytes with the low byte of `0x15A`, which is not the length, and
 * then none. */
static void check_set(set_fn f, size_t dest_off) {
    fill_guard(area, 64);
    f(area + dest_off, 9, 0x15A);
    check(all_equal(area + dest_off, 9, 0x5A));
    check(all_equal(area, dest_off, GUARD));
    check(all_equal(area + dest_off + 9, 64 - dest_off - 9, GUARD));

    fill_guard(area, 64);
    f(area + dest_off, 0, 0x15A);
    check(all_equal(area, 64, GUARD));
}

/* Clears 13 bytes, and then none. */
static void check_clr(clr_fn f, size_t dest_off) {
    fill_guard(area, 64);
    f(area + dest_off, 13);
    check(all_equal(area + dest_off, 13, 0));
    check(all_equal(area, dest_off, GUARD));
    check(all_equal(area + dest_off + 13, 64 - dest_off - 13, GUARD));

    fill_guard(area, 64);
    f(area + dest_off, 0);
    check(all_equal(area, 64, GUARD));
}

uint32_t c_check_aeabi(void) {
    checked = 0;
    failed = 0;

    /* The plain forms promise no alignment: odd offsets. The `4` and `8`
     * forms are given the alignment they promise, 8 bytes, which both need. */
    check_copy(__aeabi_memcpy, 1, 3);
    check_copy(__aeabi_memcpy4, 8, 16);
    check_copy(__aeabi_memcpy8, 8, 16);

    check_move(__aeabi_memmove, 1);
    check_move(__aeabi_memmove, 3);
    check_move(__aeabi_memmove4, 8);
    check_move(__aeabi_memmove8, 8);

    check_set(__aeabi_memset, 5);
    check_set(__aeabi_memset4, 8);
    check_set(__aeabi_memset8, 8);

    check_clr(__aeabi_memclr, 5);
    check_clr(__aeabi_memclr4, 8);
    check_clr(__aeabi_memclr8, 8);
    return failed;
}

/* How many checks `c_check_aeabi` ran, so that Erebos can tell a run that
 * checked everything from one that stopped early. */
uint32_t c_aeabi_checks(void) { return checked; }
