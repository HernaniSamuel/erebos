/* C side of c_division64.ers: divides and takes the remainder of 64-bit
 * integers. The divisors are parameters, so that clang calls the runtime's
 * functions at every optimization level; `core` defines them: libgcc's
 * `__udivdi3`, `__umoddi3`, `__divdi3` and `__moddi3` on riscv32 (ADR 0076),
 * `__aeabi_uldivmod` and `__aeabi_ldivmod` on thumbv7m (ADR 0080). The last
 * two functions take the quotient and the remainder of one pair, which the
 * EABI's single call gives both of. */
#include <stdint.h>

uint64_t c_udiv(uint64_t a, uint64_t b) { return a / b; }
uint64_t c_urem(uint64_t a, uint64_t b) { return a % b; }
int64_t c_sdiv(int64_t a, int64_t b) { return a / b; }
int64_t c_srem(int64_t a, int64_t b) { return a % b; }

uint64_t c_udivmod(uint64_t a, uint64_t b, uint64_t *rem) {
    *rem = a % b;
    return a / b;
}

int64_t c_sdivmod(int64_t a, int64_t b, int64_t *rem) {
    *rem = a % b;
    return a / b;
}
