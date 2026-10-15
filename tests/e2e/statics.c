/* C side of statics.ers: reads and writes a static that Erebos defines
 * (ADR 0030). */
#include <stdint.h>

extern uint32_t SHARED;

void c_double_shared(void) { SHARED *= 2; }

uint32_t c_read_shared(void) { return SHARED; }
