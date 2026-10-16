/* C side of enums_c.ers: the twins of its enums are C23 enums with the
 * same underlying type (ADR 0035). */
#include <stdint.h>

enum Mode : uint8_t { Mode_Off = 0, Mode_Idle = 1, Mode_Run = 3 };
enum Level : int8_t { Level_Low = -1, Level_Mid = 0, Level_High = 1 };

enum Mode erebos_next(enum Mode m);
enum Level erebos_level(_Bool high);

enum Mode c_next_mode(enum Mode m) { return m == Mode_Idle ? Mode_Run : Mode_Off; }

enum Level c_same_level(enum Level l) { return l; }

/* Relies on the caller extending `l` from 8 bits by its sign. */
int32_t c_level_as_int(enum Level l) { return l; }

uint32_t c_calls_erebos(void) {
    return (uint32_t)erebos_next(Mode_Off) * 10 + (uint32_t)erebos_next(Mode_Idle);
}

/* Relies on Erebos extending its result from 8 bits by its sign. */
int32_t c_level_from_erebos(void) { return erebos_level(0); }
