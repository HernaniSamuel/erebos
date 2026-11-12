/* C side of slice_references_c.ers: a reference to a slice crosses the C ABI
 * as the slice does, the struct `{ T *ptr; size_t len; }`, in two registers
 * (ADR 0025, 0075, 0108). The functions Erebos calls take `&[T]` and
 * `&mut [T]`, and one of Erebos's `#[export]` functions takes a `&[T]` and
 * is called from here. */
#include <stddef.h>
#include <stdint.h>

struct Bytes { const uint8_t *ptr; size_t len; };
struct MutBytes { uint8_t *ptr; size_t len; };
struct Words { const uint32_t *ptr; size_t len; };

uint32_t erebos_sum(struct Words s);
uint32_t erebos_first_plus_len(struct Bytes s);

uint32_t c_sum(struct Bytes s) {
    uint32_t sum = 0;
    for (size_t i = 0; i < s.len; i++) {
        sum += s.ptr[i];
    }
    return sum;
}

void c_fill(struct MutBytes s, uint8_t b) {
    for (size_t i = 0; i < s.len; i++) {
        s.ptr[i] = b;
    }
}

uint32_t c_sum_words(struct Words s) {
    uint32_t sum = 0;
    for (size_t i = 0; i < s.len; i++) {
        sum += s.ptr[i];
    }
    return sum;
}

/* Calls Erebos with a slice of words and one of bytes. */
uint32_t c_calls_erebos(void) {
    static const uint32_t words[4] = { 1, 2, 3, 4 };
    static const uint8_t bytes[3] = { 7, 8, 9 };
    struct Words w = { words, 4 };
    struct Bytes b = { bytes, 3 };
    return erebos_sum(w) * 100 + erebos_first_plus_len(b);
}
