/* C side of c_division64.ers: divides and takes the remainder of 64-bit
 * integers. The divisors are parameters, so that clang calls libgcc's
 * `__udivdi3`, `__umoddi3`, `__divdi3` and `__moddi3` at every
 * optimization level; `core` defines them (ADR 0076). */
#include <stdint.h>

uint64_t c_udiv(uint64_t a, uint64_t b) { return a / b; }
uint64_t c_urem(uint64_t a, uint64_t b) { return a % b; }
int64_t c_sdiv(int64_t a, int64_t b) { return a / b; }
int64_t c_srem(int64_t a, int64_t b) { return a % b; }
