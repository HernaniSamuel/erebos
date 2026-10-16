/* C side of the debug_*.ers programs, compiled with debug information of
 * several kinds: the linker script places every debug section, so none is
 * an orphan, which is a link error (ADR 0030). */
#include <stdint.h>

#define SCALE 1

struct pair {
    uint32_t a;
    uint32_t b;
};

static uint32_t helper(uint32_t x, uint32_t y) {
    uint32_t sum = 0;
    for (uint32_t i = 0; i < x; i++) {
        sum += i * y * SCALE;
    }
    return sum;
}

uint32_t c_debug(uint32_t x) {
    struct pair p = {x, x + 1};
    return helper(p.a, p.b) + p.b;
}
