/* C side of small_data.ers, compiled with -msmall-data-limit=8: its small
 * writable globals go to .sdata and .sbss, which the linker script gathers
 * with .data and .bss (ADR 0030); clang leaves the constant in .rodata. */
#include <stdint.h>

uint32_t small_initialized = 0xC0FFEE;
uint16_t small_pair[2] = {11, 22};
uint32_t small_zero;
uint64_t small_zero_wide;
const uint32_t small_constant = 0xABCD;

uint32_t c_check(void) {
    uint32_t failures = 0;
    if (small_initialized != 0xC0FFEE) failures += 1;
    if (small_pair[0] != 11 || small_pair[1] != 22) failures += 1;
    if (small_zero != 0) failures += 1;
    if (small_zero_wide != 0) failures += 1;
    if (small_constant != 0xABCD) failures += 1;
    small_zero = 5;
    return failures;
}

uint32_t c_small_zero(void) { return small_zero; }
