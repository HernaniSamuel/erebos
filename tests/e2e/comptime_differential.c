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
