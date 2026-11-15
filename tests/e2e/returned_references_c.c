/* C side of returned_references_c.ers: an `#[export]` function of Erebos that
 * returns a `&[u8]` tied to its `&[u8]` parameter is called from C, which
 * receives the C struct of a slice, `{ T *ptr; size_t len; }` (ADR 0108,
 * 0118): in two registers on riscv32 (ADR 0025), through a hidden pointer to
 * the result on thumbv7m (ADR 0075). Each function here calls it and answers
 * one fact about the result, so that Erebos prints them. */
#include <stddef.h>
#include <stdint.h>

struct Bytes { const uint8_t *ptr; size_t len; };

struct Bytes erebos_tail(struct Bytes s, size_t n);

static const uint8_t bytes[6] = { 10, 20, 30, 40, 50, 60 };

/* The tail from 2: the four bytes 30 40 50 60, inside `bytes`. */
uint32_t c_tail_len(void) {
    struct Bytes b = { bytes, 6 };
    return (uint32_t)erebos_tail(b, 2).len;
}

uint32_t c_tail_first(void) {
    struct Bytes b = { bytes, 6 };
    return erebos_tail(b, 2).ptr[0];
}

uint32_t c_tail_inside(void) {
    struct Bytes b = { bytes, 6 };
    return erebos_tail(b, 2).ptr == bytes + 2;
}

/* The tail from the length: empty, at the end of `bytes`. */
uint32_t c_empty_len(void) {
    struct Bytes b = { bytes, 6 };
    return (uint32_t)erebos_tail(b, 6).len;
}

uint32_t c_empty_inside(void) {
    struct Bytes b = { bytes, 6 };
    return erebos_tail(b, 6).ptr == bytes + 6;
}
