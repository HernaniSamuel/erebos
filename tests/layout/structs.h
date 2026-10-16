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

struct Message {
    uint8_t kind;
    struct { const uint8_t *ptr; size_t len; } text;
    struct { uint16_t *ptr; size_t len; } parts[2];
    uint16_t end;
};
