/* C side of slices.ers: a slice is the struct `{ const T *ptr; size_t len; }`
 * (ADR 0031), passed and returned in two registers (ADR 0025). */
#include <stddef.h>
#include <stdint.h>

struct Bytes { const uint8_t *ptr; size_t len; };

struct Bytes erebos_skip(struct Bytes s, size_t n);

uint32_t c_sum(struct Bytes s) {
    uint32_t sum = 0;
    for (size_t i = 0; i < s.len; i++) {
        sum += s.ptr[i];
    }
    return sum;
}

struct Bytes c_tail(struct Bytes s) {
    struct Bytes r = { s.ptr + 1, s.len - 1 };
    return r;
}

/* Seven words fill a0 to a6: the slice goes half in a7, half on the
 * stack. */
struct Bytes c_late(uint32_t a, uint32_t b, uint32_t c, uint32_t d, uint32_t e,
                    uint32_t f, uint32_t g, struct Bytes s) {
    uint32_t skip = a + b + c + d + e + f + g;
    struct Bytes r = { s.ptr + skip, s.len - skip };
    return r;
}

/* Calls Erebos with a slice, and returns the length it gives back. */
size_t c_calls_erebos(void) {
    static const uint8_t bytes[8] = { 1, 2, 3, 4, 5, 6, 7, 8 };
    struct Bytes s = { bytes, 8 };
    struct Bytes rest = erebos_skip(s, 2);
    return rest.len + rest.ptr[0];
}
