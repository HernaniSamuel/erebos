/* The C twins of the structs in `structs.ers`, for `tests/c_layout.rs`. */

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

struct S { uint8_t a; uint32_t b; uint16_t c; };
struct Wide { bool flag; uint64_t big; int8_t small; };
struct Nested { uint8_t first; struct S s; uint16_t tail[3]; };
struct Signed { int8_t a; int16_t b; int32_t c; int64_t d; intptr_t e; };
struct Pointers { uint8_t tag; struct Pointers *next; const uint16_t *data; };
struct Arrays { uint8_t bytes[5]; uint32_t words[2]; uint16_t grid[2][3]; };
struct OfStructs { struct S items[3]; uint8_t count; };

struct __attribute__((packed)) Header { uint8_t kind; uint32_t len; uint16_t tag; };

struct __attribute__((packed)) PackedNested { uint8_t a; struct S s; struct Wide w; };

struct __attribute__((aligned(64))) Descriptor { uint32_t addr; uint32_t len; };

struct __attribute__((aligned(16))) Small { uint8_t x; };

struct Mixed { uint8_t tag; struct Header header; uint16_t tail; };
struct HoldsAligned { uint8_t a; struct Descriptor d; uint8_t b; };

enum Mode : uint8_t { Mode_Off = 0, Mode_Run = 3 };
enum Port : uint16_t { Port_Data = 0x3F8 };
enum Level : int32_t { Level_Low = -1, Level_High = 1 };
struct WithEnums { enum Mode mode; enum Port port; uint8_t tag; enum Level level; enum Mode last; };

/* An enum with payloads: a union of structs that each start with the tag
 * (ADR 0039). `tests/c_layout.rs` reads the member of each variant by the
 * variant's name, and its payload values as `p0`, `p1`, ... */
union Pay8 {
    uint8_t tag;
    struct { uint8_t tag; uint8_t p0; } Byte;
    struct { uint8_t tag; uint8_t p0; uint16_t p1; } Half;
    struct { uint8_t tag; uint32_t p0; } Word;
    struct { uint8_t tag; uint64_t p0; } Long;
};
union Pay16 {
    uint16_t tag;
    struct { uint16_t tag; uint8_t p0; } A;
    struct { uint16_t tag; uint64_t p0; uint8_t p1; } B;
    struct { uint16_t tag; struct S p0; } C;
};
union Pay32 {
    uint32_t tag;
    struct { uint32_t tag; uint8_t p0; uint8_t p1; } A;
    struct { uint32_t tag; uint16_t p0; } B;
    struct { uint32_t tag; struct Wide p0; } C;
    struct { uint32_t tag; int8_t p0; struct Pointers p1; } D;
};
struct WithPayloads { uint8_t a; union Pay8 p; uint16_t b; union Pay16 q; uint8_t c; union Pay32 r; };

/* `Option(T)` and `Result(T, E)` (ADR 0045), named as `tests/c_layout.rs`
 * names them; `()` is an empty struct and `[u32; 0]` a zero-length array,
 * both of size 0 (GNU C extensions). */
union OptionU8 { uint8_t tag; struct { uint8_t tag; uint8_t p0; } Some; };
union OptionU32 { uint8_t tag; struct { uint8_t tag; uint32_t p0; } Some; };
union OptionU64 { uint8_t tag; struct { uint8_t tag; uint64_t p0; } Some; };
union OptionUnit { uint8_t tag; struct { uint8_t tag; struct {} p0; } Some; };
union OptionU32x0 { uint8_t tag; struct { uint8_t tag; uint32_t p0[0]; } Some; };
union ResultU32Mode {
    uint8_t tag;
    struct { uint8_t tag; uint32_t p0; } Ok;
    struct { uint8_t tag; enum Mode p0; } Err;
};
struct WithOptions {
    union OptionU8 a;
    union OptionU32 b;
    union OptionU64 c;
    union OptionUnit d;
    union ResultU32Mode e;
    union OptionU32x0 f;
};

struct Message {
    uint8_t kind;
    struct { const uint8_t *ptr; size_t len; } text;
    struct { uint16_t *ptr; size_t len; } parts[2];
    uint16_t end;
};

/* `regs::Block`, of the module `regs`, with private and `pub` fields
 * (ADR 0050); `::` is `__` in C. */
struct regs__Block { uint32_t ctrl; uint8_t status; uint16_t data; uint8_t reserved[3]; uint64_t last; };
