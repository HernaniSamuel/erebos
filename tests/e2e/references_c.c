/* C side of references_c.ers: functions Erebos calls with references, which
 * are plain pointers to C, and one that calls back an `#[export]` function
 * of Erebos with addresses (ADR 0105). */
#include <stdint.h>

struct Pair {
    uint32_t a;
    uint32_t b;
};

void erebos_scale(uint32_t *out, const uint32_t *input);

uint32_t c_add_pair(uint32_t *acc, const struct Pair *p) {
    *acc += p->a + p->b;
    return *acc;
}

void c_forward(uint32_t *out, const uint32_t *input) {
    erebos_scale(out, input);
}

uint32_t c_roundtrip(uint32_t seed) {
    uint32_t input = seed;
    uint32_t out = 0;
    erebos_scale(&out, &input);
    return out + input;
}
