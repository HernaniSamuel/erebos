/* C side of naked_riscv32.ers: calls the naked function the program exports
 * by its own name, with the C ABI (ADR 0079). */
#include <stdint.h>

uint32_t erebos_sum(uint32_t a, uint32_t b);

uint32_t c_sum_plus_one(uint32_t a, uint32_t b) { return erebos_sum(a, b) + 1; }
