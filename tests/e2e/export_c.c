/* C side of export_c.ers: reads an `#[export]` static and calls an
 * `#[export]` function of an Erebos submodule by their own names
 * (ADR 0051). */
#include <stdint.h>

extern uint32_t ticks;
uint32_t erebos_double(uint32_t n);

uint32_t c_read_ticks(void) { return ticks; }

uint32_t c_double_plus_one(uint32_t n) { return erebos_double(n) + 1; }
