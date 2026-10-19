/* C side of options_c.ers: the twins of `Option(T)` and `Result(T, E)`
 * are unions of structs that each start with the `uint8_t` tag (ADR 0045),
 * which clang passes by their size and alignment, as Erebos does
 * (ADR 0025). */
#include <stdint.h>

enum Fault : uint8_t { Fault_Timeout = 3, Fault_Busy = 7 };

union OptionU8 {
    uint8_t tag;                                  /* 0: None */
    struct { uint8_t tag; uint8_t value; } some;  /* 1: Some */
};

union OptionU32 {
    uint8_t tag;
    struct { uint8_t tag; uint32_t value; } some;
};

union OptionU64 {
    uint8_t tag;
    struct { uint8_t tag; uint64_t value; } some;
};

union ResultU32Fault {
    uint8_t tag;                                     /* 0: Ok, 1: Err */
    struct { uint8_t tag; uint32_t value; } ok;
    struct { uint8_t tag; enum Fault error; } err;
};

union OptionU8 erebos_u8(union OptionU8 x);
union OptionU32 erebos_u32(union OptionU32 x);
union OptionU64 erebos_u64(union OptionU64 x);
union ResultU32Fault erebos_result(union ResultU32Fault x);

/* Some(0) becomes None, Some(v) becomes Some(v + 1), and None becomes
 * Some(42). */
union OptionU8 c_u8(union OptionU8 x) {
    union OptionU8 r;
    if (x.tag == 1 && x.some.value != 0) {
        r.some.tag = 1;
        r.some.value = x.some.value + 1;
    } else if (x.tag == 1) {
        r.tag = 0;
    } else {
        r.some.tag = 1;
        r.some.value = 42;
    }
    return r;
}

/* As c_u8, None becoming Some(70000). */
union OptionU32 c_u32(union OptionU32 x) {
    union OptionU32 r;
    if (x.tag == 1 && x.some.value != 0) {
        r.some.tag = 1;
        r.some.value = x.some.value + 1;
    } else if (x.tag == 1) {
        r.tag = 0;
    } else {
        r.some.tag = 1;
        r.some.value = 70000;
    }
    return r;
}

/* As c_u8, None becoming Some(UINT64_MAX). */
union OptionU64 c_u64(union OptionU64 x) {
    union OptionU64 r;
    if (x.tag == 1 && x.some.value != 0) {
        r.some.tag = 1;
        r.some.value = x.some.value + 1;
    } else if (x.tag == 1) {
        r.tag = 0;
    } else {
        r.some.tag = 1;
        r.some.value = UINT64_MAX;
    }
    return r;
}

/* Ok(0) becomes Err(Busy), Ok(v) becomes Ok(v + 1), and Err(e) becomes
 * Ok(e * 100). */
union ResultU32Fault c_result(union ResultU32Fault x) {
    union ResultU32Fault r;
    if (x.tag == 0 && x.ok.value != 0) {
        r.ok.tag = 0;
        r.ok.value = x.ok.value + 1;
    } else if (x.tag == 0) {
        r.err.tag = 1;
        r.err.error = Fault_Busy;
    } else {
        r.ok.tag = 0;
        r.ok.value = (uint32_t)x.err.error * 100;
    }
    return r;
}

/* Each builds the union with the tag and, for Some, the value given, calls
 * the Erebos function, and writes out the tag and the payload value of
 * what it returns, 0 for None. */
void c_sees_u8(uint8_t tag, uint8_t value, uint8_t *out_tag, uint64_t *out_value) {
    union OptionU8 x;
    if (tag == 1) {
        x.some.tag = 1;
        x.some.value = value;
    } else {
        x.tag = 0;
    }
    union OptionU8 r = erebos_u8(x);
    *out_tag = r.tag;
    *out_value = r.tag == 1 ? r.some.value : 0;
}

void c_sees_u32(uint8_t tag, uint32_t value, uint8_t *out_tag, uint64_t *out_value) {
    union OptionU32 x;
    if (tag == 1) {
        x.some.tag = 1;
        x.some.value = value;
    } else {
        x.tag = 0;
    }
    union OptionU32 r = erebos_u32(x);
    *out_tag = r.tag;
    *out_value = r.tag == 1 ? r.some.value : 0;
}

void c_sees_u64(uint8_t tag, uint64_t value, uint8_t *out_tag, uint64_t *out_value) {
    union OptionU64 x;
    if (tag == 1) {
        x.some.tag = 1;
        x.some.value = value;
    } else {
        x.tag = 0;
    }
    union OptionU64 r = erebos_u64(x);
    *out_tag = r.tag;
    *out_value = r.tag == 1 ? r.some.value : 0;
}

/* For Ok, `value` is the u32; for Err, its low byte is the Fault. */
void c_sees_result(uint8_t tag, uint32_t value, uint8_t *out_tag, uint64_t *out_value) {
    union ResultU32Fault x;
    if (tag == 0) {
        x.ok.tag = 0;
        x.ok.value = value;
    } else {
        x.err.tag = 1;
        x.err.error = (enum Fault)value;
    }
    union ResultU32Fault r = erebos_result(x);
    *out_tag = r.tag;
    *out_value = r.tag == 0 ? r.ok.value : (uint64_t)r.err.error;
}
