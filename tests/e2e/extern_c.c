/* C side of extern_c.ers: functions Erebos calls, and one that calls back
 * into Erebos (ADR 0026). */
#include <stdint.h>

uint32_t erebos_square(uint32_t x);

uint32_t c_add(uint32_t a, uint32_t b) { return a + b; }

int16_t c_negate(int16_t x) { return (int16_t)-x; }

uint64_t c_mul64(uint64_t a, uint64_t b) { return a * b; }

uint32_t c_sum_squares(uint32_t n) {
    uint32_t sum = 0;
    for (uint32_t i = 1; i <= n; i++) {
        sum += erebos_square(i);
    }
    return sum;
}

void c_fill(uint8_t *buf, uint32_t n, uint8_t value) {
    for (uint32_t i = 0; i < n; i++) {
        buf[i] = value;
    }
}
