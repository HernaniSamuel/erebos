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

/* The twins of the program's struct `Pair` and its enums with payloads,
 * as unions of structs that each start with the tag (ADR 0039). */
struct Pair { uint32_t a; uint32_t b; };
union Small {
    uint8_t tag;
    struct { uint8_t tag; uint8_t p0; } On;
};
union Packet {
    uint16_t tag;
    struct { uint16_t tag; uint8_t p0; } Byte;
    struct { uint16_t tag; uint8_t p0; uint32_t p1; } Two;
    struct { uint16_t tag; _Bool p0; enum Mode p1; } Flags;
    struct { uint16_t tag; uint8_t p0[3]; int16_t p1; } Data;
    struct { uint16_t tag; struct Pair p0; } Nested;
    struct { uint16_t tag; union Small p0; } Inner;
    struct { uint16_t tag; uint8_t p0; } Zero;
};

union Packet opaque_packet(union Packet p) { return p; }

/* The twin of the program's enum `Widths`. */
union Widths {
    uint8_t tag;
    struct { uint8_t tag; uint16_t p0; uint64_t p1; } U;
    struct { uint8_t tag; int8_t p0; int32_t p1; int64_t p2; } S;
    struct { uint8_t tag; enum Level p0; enum Code p1; enum Port p2; } E;
};

union Widths opaque_widths(union Widths w) { return w; }

/* The twins of the program's enums `Side` and `Duo`. */
union Side {
    uint8_t tag;
    struct { uint8_t tag; uint8_t p0; } Left;
    struct { uint8_t tag; uint8_t p0; } Right;
    struct { uint8_t tag; uint16_t p0; } Wide;
};
union Duo {
    uint16_t tag;
    struct { uint16_t tag; uint8_t p0; uint8_t p1; } Pair;
    struct { uint16_t tag; uint8_t p0; } One;
    struct { uint16_t tag; union Side p0; uint8_t p1; } Wrap;
};

union Duo opaque_duo(union Duo d) { return d; }

/* The twin of the program's bit-field struct `Line` is its integer type
 * (ADR 0059). */
uint8_t opaque_line(uint8_t l) { return l; }
