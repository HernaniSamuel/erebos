/* C side of extern_statics.ers: a variable Erebos reads and writes, and a
 * table it reads, both defined here (ADR 0030). */
#include <stdint.h>

uint32_t rx_count = 40;
const uint16_t crc_lookup[4] = {0x1021, 0x2042, 0x3063, 0x4084};

uint32_t c_rx_count(void) { return rx_count; }
