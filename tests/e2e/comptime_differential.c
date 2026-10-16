/* Identity functions for comptime_differential.ers. Compiled apart from the
 * program, they hide their result from LLVM, so the run-time side of each
 * comparison really runs on the target instead of being folded. */
#include <stdint.h>

uint8_t opaque_u8(uint8_t x) { return x; }
uint16_t opaque_u16(uint16_t x) { return x; }
uint32_t opaque_u32(uint32_t x) { return x; }
uint64_t opaque_u64(uint64_t x) { return x; }
int8_t opaque_i8(int8_t x) { return x; }
int16_t opaque_i16(int16_t x) { return x; }
int32_t opaque_i32(int32_t x) { return x; }
int64_t opaque_i64(int64_t x) { return x; }
uint32_t opaque_usize(uint32_t x) { return x; }

/* The twins of the program's enums, as C23 enums (ADR 0035). */
enum Mode : uint8_t { Mode_Off = 0, Mode_Run = 3, Mode_Max = 255 };
enum Level : int8_t { Level_Low = -128, Level_Minus = -1, Level_High = 127 };
enum Port : uint16_t { Port_Data = 0x3F8, Port_Top = 0xFFFF };
enum Code : int32_t { Code_Fault = INT32_MIN, Code_Neg = -70000, Code_Fine = 70000 };

enum Mode opaque_mode(enum Mode m) { return m; }
enum Level opaque_level(enum Level l) { return l; }
enum Port opaque_port(enum Port p) { return p; }
enum Code opaque_code(enum Code c) { return c; }
