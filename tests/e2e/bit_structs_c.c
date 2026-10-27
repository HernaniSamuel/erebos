/* C side of bit_structs_c.ers: the twin of a bit-field struct is its
 * integer type (ADR 0059), `uint8_t`, `uint16_t` or `uint32_t`. */
#include <stdint.h>

uint8_t erebos_lcr(_Bool dlab);
uint16_t erebos_status(uint8_t count);
uint32_t erebos_status_count(uint16_t s);
uint32_t erebos_control(uint32_t c);

uint8_t c_flip_dlab(uint8_t l) { return l ^ 0x80; }

/* Relies on the caller extending `l` from 8 bits with zeros. */
uint32_t c_lcr_as_int(uint8_t l) { return l; }

uint16_t c_status(uint32_t count) { return (uint16_t)(0x8000 | count); }

/* Relies on the caller extending `s` from 16 bits with zeros. */
uint32_t c_status_as_int(uint16_t s) { return s; }

uint32_t c_rotate(uint32_t c) { return (c << 8) | (c >> 24); }

/* Relies on Erebos extending its result from 8 bits with zeros. */
uint32_t c_lcr_from_erebos(void) { return erebos_lcr(1); }

/* Relies on Erebos extending its result from 16 bits with zeros. */
uint32_t c_status_from_erebos(void) { return erebos_status(3); }

uint32_t c_count_in_erebos(void) { return erebos_status_count(0x800C); }

uint32_t c_control_from_erebos(void) { return erebos_control(0x12345678); }
