//! Generator of valid integer programs (ADR 0068, section 1).
//!
//! From a seed it writes functions `f0`, `f1`, ... over the integer types
//! and `bool`, each taking a few parameters and returning a `u64` hash of
//! what it computes. The first version has only integers and control flow.
//!
//! A generated program is valid: it type-checks and lowers to MIR without a
//! diagnostic. The rules that keep it so:
//!
//! - A literal takes its type from its position, so where no type is
//!   expected (the operand of a cast, the left of a comparison, a `match`
//!   scrutinee, a shift amount, what `h` folds in) the generator asks for an
//!   expression that is *not known at compile time*, and gives it a typed
//!   leftmost operand: the left of a binary operator, the first branch of
//!   an `if` and the first arm of a `match` read a variable, and only the
//!   operands after them may be literals.
//! - The checks on values known at compile time (ADR 0032) never fire. A
//!   shift amount is a literal in range or an unknown value, and a divisor
//!   is `(e | 1)`, which is never zero, and is `-1` for some `e` of a
//!   signed type, so `MIN / -1` is reached. The unknown operands of a
//!   program are its parameters and what is computed from them.
//! - Loops run a count of times fixed by their counter, which the body
//!   never assigns.
//!
//! The generator records which operators, types and constructs it used in
//! `Program::uses`, so that a test can demand that a run exercises all of
//! them.

use std::fmt::Write as _;

/// xorshift64, as `tests/fuzz.rs` has it, seeded through one step of
/// splitmix64 so that consecutive seeds give unrelated programs.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        let mut z = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        // xorshift never leaves zero.
        Rng(z.max(1))
    }

    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    /// A number below `n` (`n` of 0 counts as 1).
    pub fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }

    /// True `num` times in `den`.
    pub fn chance(&mut self, num: usize, den: usize) -> bool {
        self.below(den) < num
    }

    pub fn pick<T: Copy>(&mut self, items: &[T]) -> T {
        items[self.below(items.len())]
    }
}

/// The types of the generated programs. `usize` and `isize` are 32 bits
/// wide, as on the target.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Ty {
    U8,
    U16,
    U32,
    U64,
    I8,
    I16,
    I32,
    I64,
    Usize,
    Isize,
    Bool,
}

impl Ty {
    pub const INTS: [Ty; 10] = [
        Ty::U8,
        Ty::U16,
        Ty::U32,
        Ty::U64,
        Ty::I8,
        Ty::I16,
        Ty::I32,
        Ty::I64,
        Ty::Usize,
        Ty::Isize,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Ty::U8 => "u8",
            Ty::U16 => "u16",
            Ty::U32 => "u32",
            Ty::U64 => "u64",
            Ty::I8 => "i8",
            Ty::I16 => "i16",
            Ty::I32 => "i32",
            Ty::I64 => "i64",
            Ty::Usize => "usize",
            Ty::Isize => "isize",
            Ty::Bool => "bool",
        }
    }

    pub fn bits(self) -> u32 {
        match self {
            Ty::U8 | Ty::I8 => 8,
            Ty::U16 | Ty::I16 => 16,
            Ty::U32 | Ty::I32 | Ty::Usize | Ty::Isize => 32,
            Ty::U64 | Ty::I64 => 64,
            Ty::Bool => 1,
        }
    }

    pub fn signed(self) -> bool {
        matches!(self, Ty::I8 | Ty::I16 | Ty::I32 | Ty::I64 | Ty::Isize)
    }

    pub fn min(self) -> i128 {
        if self.signed() {
            -(1i128 << (self.bits() - 1))
        } else {
            0
        }
    }

    pub fn max(self) -> i128 {
        if self.signed() {
            (1i128 << (self.bits() - 1)) - 1
        } else {
            (1i128 << self.bits()) - 1
        }
    }

    fn feature(self) -> Feature {
        match self {
            Ty::U8 => Feature::TyU8,
            Ty::U16 => Feature::TyU16,
            Ty::U32 => Feature::TyU32,
            Ty::U64 => Feature::TyU64,
            Ty::I8 => Feature::TyI8,
            Ty::I16 => Feature::TyI16,
            Ty::I32 => Feature::TyI32,
            Ty::I64 => Feature::TyI64,
            Ty::Usize => Feature::TyUsize,
            Ty::Isize => Feature::TyIsize,
            Ty::Bool => Feature::TyBool,
        }
    }
}

/// What a program can use: every operator, type and construct of ADR 0068,
/// section 1.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Feature {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Shl,
    Shr,
    BitAnd,
    BitOr,
    BitXor,
    ShiftByLiteral,
    ShiftByValue,
    Neg,
    NotInt,
    NotBool,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Cast,
    CastBool,
    Let,
    LetMut,
    Assign,
    CompoundAssign,
    IfStmt,
    IfElseStmt,
    IfExpr,
    Match,
    MatchBind,
    MatchOr,
    While,
    Loop,
    Break,
    TyU8,
    TyU16,
    TyU32,
    TyU64,
    TyI8,
    TyI16,
    TyI32,
    TyI64,
    TyUsize,
    TyIsize,
    TyBool,
}

impl Feature {
    pub const COUNT: usize = Feature::TyBool as usize + 1;

    pub const ALL: [Feature; Feature::COUNT] = [
        Feature::Add,
        Feature::Sub,
        Feature::Mul,
        Feature::Div,
        Feature::Rem,
        Feature::Shl,
        Feature::Shr,
        Feature::BitAnd,
        Feature::BitOr,
        Feature::BitXor,
        Feature::ShiftByLiteral,
        Feature::ShiftByValue,
        Feature::Neg,
        Feature::NotInt,
        Feature::NotBool,
        Feature::Eq,
        Feature::Ne,
        Feature::Lt,
        Feature::Le,
        Feature::Gt,
        Feature::Ge,
        Feature::Cast,
        Feature::CastBool,
        Feature::Let,
        Feature::LetMut,
        Feature::Assign,
        Feature::CompoundAssign,
        Feature::IfStmt,
        Feature::IfElseStmt,
        Feature::IfExpr,
        Feature::Match,
        Feature::MatchBind,
        Feature::MatchOr,
        Feature::While,
        Feature::Loop,
        Feature::Break,
        Feature::TyU8,
        Feature::TyU16,
        Feature::TyU32,
        Feature::TyU64,
        Feature::TyI8,
        Feature::TyI16,
        Feature::TyI32,
        Feature::TyI64,
        Feature::TyUsize,
        Feature::TyIsize,
        Feature::TyBool,
    ];
}

/// A set of features.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Uses(u64);

impl Uses {
    pub fn with(self, feature: Feature) -> Uses {
        Uses(self.0 | 1 << feature as u32)
    }

    pub fn union(self, other: Uses) -> Uses {
        Uses(self.0 | other.0)
    }

    pub fn has(self, feature: Feature) -> bool {
        self.0 >> feature as u32 & 1 == 1
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Bin {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Shl,
    Shr,
    And,
    Or,
    Xor,
}

impl Bin {
    const ALL: [Bin; 10] = [
        Bin::Add,
        Bin::Sub,
        Bin::Mul,
        Bin::Div,
        Bin::Rem,
        Bin::Shl,
        Bin::Shr,
        Bin::And,
        Bin::Or,
        Bin::Xor,
    ];

    fn symbol(self) -> &'static str {
        match self {
            Bin::Add => "+",
            Bin::Sub => "-",
            Bin::Mul => "*",
            Bin::Div => "/",
            Bin::Rem => "%",
            Bin::Shl => "<<",
            Bin::Shr => ">>",
            Bin::And => "&",
            Bin::Or => "|",
            Bin::Xor => "^",
        }
    }

    fn feature(self) -> Feature {
        match self {
            Bin::Add => Feature::Add,
            Bin::Sub => Feature::Sub,
            Bin::Mul => Feature::Mul,
            Bin::Div => Feature::Div,
            Bin::Rem => Feature::Rem,
            Bin::Shl => Feature::Shl,
            Bin::Shr => Feature::Shr,
            Bin::And => Feature::BitAnd,
            Bin::Or => Feature::BitOr,
            Bin::Xor => Feature::BitXor,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Cmp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

impl Cmp {
    const ALL: [Cmp; 6] = [Cmp::Eq, Cmp::Ne, Cmp::Lt, Cmp::Le, Cmp::Gt, Cmp::Ge];

    fn symbol(self) -> &'static str {
        match self {
            Cmp::Eq => "==",
            Cmp::Ne => "!=",
            Cmp::Lt => "<",
            Cmp::Le => "<=",
            Cmp::Gt => ">",
            Cmp::Ge => ">=",
        }
    }

    fn feature(self) -> Feature {
        match self {
            Cmp::Eq => Feature::Eq,
            Cmp::Ne => Feature::Ne,
            Cmp::Lt => Feature::Lt,
            Cmp::Le => Feature::Le,
            Cmp::Gt => Feature::Gt,
            Cmp::Ge => Feature::Ge,
        }
    }
}

/// A function of a generated program.
#[derive(Clone, Debug)]
pub struct Function {
    pub name: String,
    pub params: Vec<Ty>,
}

/// A generated program and what is known about it.
#[derive(Clone, Debug)]
pub struct Program {
    pub seed: u64,
    pub source: String,
    pub functions: Vec<Function>,
    pub uses: Uses,
}

/// The C file a generated program links: identity functions that hide
/// their result from LLVM, so that a call with their results as arguments
/// really runs on the target (ADR 0068, section 2). As in
/// `comptime_differential.c`, `usize` is `uint32_t`.
pub const OPAQUE_C: &str = "\
/* Identity functions for generated programs. Compiled apart from the
 * program, they hide their result from LLVM. */
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
int32_t opaque_isize(int32_t x) { return x; }
";

/// The program of `seed`.
pub fn generate(seed: u64) -> Program {
    let mut g = Gen {
        rng: Rng::new(seed),
        vars: Vec::new(),
        counter: 0,
        used: Uses::default(),
    };
    let mut source = format!("// Generated program, seed {seed} (ADR 0068).\n");
    for ty in Ty::INTS {
        let n = ty.name();
        let _ = writeln!(source, "extern fn opaque_{n}(x: {n}) -> {n};");
    }
    let mut functions = Vec::new();
    for i in 0..2 + g.rng.below(3) {
        let (function, text) = g.function(i);
        source.push('\n');
        source.push_str(&text);
        functions.push(function);
    }
    source.push_str(&g.main(&functions));
    Program {
        seed,
        source,
        functions,
        uses: g.used,
    }
}

#[derive(Clone)]
struct Var {
    name: String,
    ty: Ty,
    mutable: bool,
}

/// An expression: its text (parenthesized when not atomic), whether it is
/// known at compile time, and the features it uses. The features travel
/// with the text because the generator may drop an expression it made.
struct Ex {
    text: String,
    known: bool,
    uses: Uses,
}

impl Ex {
    fn new(text: impl Into<String>, known: bool) -> Self {
        Ex {
            text: text.into(),
            known,
            uses: Uses::default(),
        }
    }

    fn using(mut self, features: &[Feature]) -> Self {
        for f in features {
            self.uses = self.uses.with(*f);
        }
        self
    }

    fn and(mut self, other: &Ex) -> Self {
        self.uses = self.uses.union(other.uses);
        self
    }
}

const EXPR_DEPTH: u32 = 3;
const BLOCK_DEPTH: u32 = 2;

struct Gen {
    rng: Rng,
    vars: Vec<Var>,
    counter: usize,
    used: Uses,
}

impl Gen {
    fn fresh(&mut self, prefix: &str) -> String {
        self.counter += 1;
        format!("{prefix}{}", self.counter)
    }

    /// A literal of `ty` for an argument: an edge value or a random one.
    fn input(&mut self, ty: Ty) -> String {
        if ty == Ty::Bool {
            return if self.rng.chance(1, 2) {
                "true"
            } else {
                "false"
            }
            .into();
        }
        let bits = ty.bits();
        let value: i128 = match self.rng.below(8) {
            0 => 0,
            1 => 1,
            2 => ty.max(),
            3 => ty.min(),
            4 => {
                if ty.signed() {
                    -1
                } else {
                    ty.max() - 1
                }
            }
            5 => (self.rng.below(20) as i128 - 10).max(ty.min()),
            _ => {
                let raw = self.rng.next() as i128 & ((1i128 << bits) - 1);
                if ty.signed() && raw > ty.max() {
                    raw - (1i128 << bits)
                } else {
                    raw
                }
            }
        };
        value.to_string()
    }

    /// The consts, `main` and `on_panic` of ADR 0068, section 2, and the
    /// `memory` block QEMU needs. Each function is called with two sets of
    /// literals, at compile time in `const CT_<f>_<k>: u64 = comptime ...;`
    /// and at run time with the same values through the `opaque_*`
    /// functions of `OPAQUE_C`. `main` stops with `generated ok` and exit
    /// code 0, or with the number (from 1) of the first function whose two
    /// results differ, after printing `#<number> <comptime> <run time>`.
    fn main(&mut self, functions: &[Function]) -> String {
        let mut consts = String::new();
        let mut body = String::new();
        for (i, function) in functions.iter().enumerate() {
            for k in 0..2 {
                let mut compile_time = Vec::new();
                let mut run_time = Vec::new();
                for ty in &function.params {
                    let lit = self.input(*ty);
                    run_time.push(match ty {
                        Ty::Bool => format!("opaque_u8({}) == 1", u8::from(lit == "true")),
                        _ => format!("opaque_{}({lit})", ty.name()),
                    });
                    compile_time.push(lit);
                }
                let _ = writeln!(
                    consts,
                    "const CT_{i}_{k}: u64 = comptime {}({});",
                    function.name,
                    compile_time.join(", ")
                );
                let _ = writeln!(
                    body,
                    "    if bad == 0 {{ bad = check({}, CT_{i}_{k}, {}({})); }}",
                    i + 1,
                    function.name,
                    run_time.join(", ")
                );
            }
        }
        format!(
            "
{consts}
mod board;

fn print_u64(n: u64) {{
    if n >= 10 {{
        print_u64(n / 10);
    }}
    board::putc(b'0' + (n % 10) as u8);
}}

fn check(id: u32, at_compile_time: u64, at_run_time: u64) -> u32 {{
    if at_compile_time == at_run_time {{
        return 0;
    }}
    board::putc(b'#');
    print_u64(id as u64);
    board::putc(b' ');
    print_u64(at_compile_time);
    board::putc(b' ');
    print_u64(at_run_time);
    board::putc(b'\\n');
    id
}}

entry fn main() -> ! {{
    let mut bad: u32 = 0;
{body}    if bad == 0 {{
        board::putc(b'g'); board::putc(b'e'); board::putc(b'n'); board::putc(b'e');
        board::putc(b'r'); board::putc(b'a'); board::putc(b't'); board::putc(b'e');
        board::putc(b'd'); board::putc(b' '); board::putc(b'o'); board::putc(b'k');
        board::putc(b'\\n');
        board::exit(0)
    }} else {{
        board::exit(bad)
    }}
}}

panic fn on_panic() -> ! {{
    board::exit(99)
}}
"
        )
    }

    fn use_feature(&mut self, feature: Feature) {
        self.used = self.used.with(feature);
    }

    fn function(&mut self, index: usize) -> (Function, String) {
        self.vars.clear();
        let mut params = Vec::new();
        let mut text = String::new();
        let count = 2 + self.rng.below(3);
        for i in 0..count {
            // The first parameter is an integer, so every function has an
            // unknown integer to compute from.
            let ty = if i > 0 && self.rng.chance(1, 6) {
                Ty::Bool
            } else {
                self.rng.pick(&Ty::INTS)
            };
            params.push(ty);
            self.use_feature(ty.feature());
            self.vars.push(Var {
                name: format!("p{i}"),
                ty,
                mutable: false,
            });
        }
        let list: Vec<String> = params
            .iter()
            .enumerate()
            .map(|(i, ty)| format!("p{i}: {}", ty.name()))
            .collect();
        let _ = writeln!(text, "fn f{index}({}) -> u64 {{", list.join(", "));
        text.push_str("    let mut h: u64 = 0;\n");
        for _ in 0..6 + self.rng.below(7) {
            self.stmt(BLOCK_DEPTH, 1, &mut text);
        }
        // Everything declared at the top of the function is folded in.
        let top: Vec<Var> = self.vars.clone();
        for var in top {
            if self.rng.chance(2, 3) {
                let ex = Ex::new(var.name.clone(), false);
                self.observe(&ex, 1, &mut text);
            }
        }
        text.push_str("    h\n}\n");
        (
            Function {
                name: format!("f{index}"),
                params,
            },
            text,
        )
    }

    /// `h = h * 1_000_003 + (e as u64)`. The cast is not one of the
    /// program's casts, so it counts for no feature.
    fn observe(&mut self, ex: &Ex, indent: usize, out: &mut String) {
        self.used = self.used.union(ex.uses);
        let pad = "    ".repeat(indent);
        let _ = writeln!(out, "{pad}h = h * 1_000_003 + ({} as u64);", ex.text);
    }

    fn block(&mut self, depth: u32, indent: usize, out: &mut String) {
        let mark = self.vars.len();
        for _ in 0..1 + self.rng.below(3) {
            self.stmt(depth, indent, out);
        }
        self.vars.truncate(mark);
    }

    fn stmt(&mut self, depth: u32, indent: usize, out: &mut String) {
        let pad = "    ".repeat(indent);
        match self.rng.below(20) {
            0..=6 => self.let_stmt(&pad, out),
            7..=10 => {
                let mutable: Vec<Var> = self.vars.iter().filter(|v| v.mutable).cloned().collect();
                if mutable.is_empty() {
                    return self.let_stmt(&pad, out);
                }
                let var = mutable[self.rng.below(mutable.len())].clone();
                self.assign_stmt(&var, &pad, out);
            }
            11..=13 => {
                let ty = if self.rng.chance(1, 8) {
                    Ty::Bool
                } else {
                    self.rng.pick(&Ty::INTS)
                };
                let ex = self.expr(ty, EXPR_DEPTH, true);
                self.observe(&ex, indent, out);
            }
            14..=16 if depth > 0 => self.if_stmt(depth, indent, out),
            17..=19 if depth > 0 => self.loop_stmt(depth, indent, out),
            _ => self.let_stmt(&pad, out),
        }
    }

    fn let_stmt(&mut self, pad: &str, out: &mut String) {
        let ty = if self.rng.chance(1, 8) {
            Ty::Bool
        } else {
            self.rng.pick(&Ty::INTS)
        };
        let mutable = self.rng.chance(1, 2);
        let ex = self.expr(ty, EXPR_DEPTH, false);
        self.used = self.used.union(ex.uses);
        self.use_feature(ty.feature());
        self.use_feature(if mutable {
            Feature::LetMut
        } else {
            Feature::Let
        });
        let name = self.fresh("v");
        let kw = if mutable { "let mut" } else { "let" };
        let _ = writeln!(out, "{pad}{kw} {name}: {} = {};", ty.name(), ex.text);
        self.vars.push(Var { name, ty, mutable });
    }

    fn assign_stmt(&mut self, var: &Var, pad: &str, out: &mut String) {
        if var.ty != Ty::Bool && self.rng.chance(1, 2) {
            let op = self.rng.pick(&Bin::ALL);
            let rhs = self.operand(op, var.ty, EXPR_DEPTH - 1);
            self.used = self.used.union(rhs.uses);
            self.use_feature(Feature::CompoundAssign);
            self.use_feature(op.feature());
            let _ = writeln!(out, "{pad}{} {}= {};", var.name, op.symbol(), rhs.text);
        } else {
            let ex = self.expr(var.ty, EXPR_DEPTH, false);
            self.used = self.used.union(ex.uses);
            self.use_feature(Feature::Assign);
            let _ = writeln!(out, "{pad}{} = {};", var.name, ex.text);
        }
    }

    fn if_stmt(&mut self, depth: u32, indent: usize, out: &mut String) {
        let pad = "    ".repeat(indent);
        let cond = self.bool_expr(2, false);
        self.used = self.used.union(cond.uses);
        self.use_feature(Feature::IfStmt);
        let _ = writeln!(out, "{pad}if {} {{", cond.text);
        self.block(depth - 1, indent + 1, out);
        if self.rng.chance(2, 3) {
            self.use_feature(Feature::IfElseStmt);
            let _ = writeln!(out, "{pad}}} else {{");
            self.block(depth - 1, indent + 1, out);
        }
        let _ = writeln!(out, "{pad}}}");
    }

    /// A `while` or a `loop` run `bound` times by a counter that the body
    /// can read but not assign.
    fn loop_stmt(&mut self, depth: u32, indent: usize, out: &mut String) {
        let pad = "    ".repeat(indent);
        let counter = self.fresh("i");
        let bound = 1 + self.rng.below(4);
        self.use_feature(Ty::U8.feature());
        let _ = writeln!(out, "{pad}let mut {counter}: u8 = 0;");
        let mark = self.vars.len();
        self.vars.push(Var {
            name: counter.clone(),
            ty: Ty::U8,
            mutable: false,
        });
        if self.rng.chance(1, 2) {
            self.use_feature(Feature::While);
            let _ = writeln!(out, "{pad}while {counter} < {bound} {{");
        } else {
            self.use_feature(Feature::Loop);
            self.use_feature(Feature::Break);
            let _ = writeln!(out, "{pad}loop {{");
            let _ = writeln!(out, "{pad}    if {counter} >= {bound} {{");
            let _ = writeln!(out, "{pad}        break;");
            let _ = writeln!(out, "{pad}    }}");
        }
        self.block(depth - 1, indent + 1, out);
        let _ = writeln!(out, "{pad}    {counter} += 1;");
        let _ = writeln!(out, "{pad}}}");
        self.vars.truncate(mark);
    }

    // Expressions.

    fn expr(&mut self, ty: Ty, depth: u32, unknown: bool) -> Ex {
        if ty == Ty::Bool {
            self.bool_expr(depth, unknown)
        } else {
            self.int_expr(ty, depth, unknown)
        }
    }

    /// An integer expression of type `ty`. With `unknown`, one that is not
    /// known at compile time.
    fn int_expr(&mut self, ty: Ty, depth: u32, unknown: bool) -> Ex {
        if depth == 0 {
            return self.leaf(ty, unknown);
        }
        let d = depth - 1;
        let ex = match self.rng.below(13) {
            0..=2 => self.leaf(ty, unknown),
            3..=6 => self.binary(ty, d, unknown),
            7 if ty.signed() => {
                let a = self.int_expr(ty, d, unknown);
                Ex::new(format!("(-{})", a.text), a.known)
                    .and(&a)
                    .using(&[Feature::Neg])
            }
            7 | 8 => {
                let a = self.int_expr(ty, d, unknown);
                Ex::new(format!("(!{})", a.text), a.known)
                    .and(&a)
                    .using(&[Feature::NotInt])
            }
            9 | 10 => self.cast_to(ty, d),
            11 => self.if_expr(ty, d, unknown),
            _ => self.match_expr(ty, d, unknown),
        };
        if unknown && ex.known {
            self.unknown_leaf(ty)
        } else {
            ex
        }
    }

    fn binary(&mut self, ty: Ty, depth: u32, unknown: bool) -> Ex {
        let op = self.rng.pick(&Bin::ALL);
        let a = self.int_expr(ty, depth, unknown);
        let b = self.operand(op, ty, depth);
        let known = a.known && b.known;
        Ex::new(format!("({} {} {})", a.text, op.symbol(), b.text), known)
            .and(&a)
            .and(&b)
            .using(&[op.feature()])
    }

    /// The right operand of `op` on a left operand of type `ty`. A divisor
    /// is `(e | 1)`, never zero; a shift amount is a literal in range or an
    /// unknown value of any integer type, which the target masks.
    fn operand(&mut self, op: Bin, ty: Ty, depth: u32) -> Ex {
        match op {
            Bin::Div | Bin::Rem => {
                let b = self.int_expr(ty, depth, false);
                Ex::new(format!("({} | 1)", b.text), b.known).and(&b)
            }
            Bin::Shl | Bin::Shr => {
                if self.rng.chance(1, 2) {
                    let amount = self.rng.below(ty.bits() as usize);
                    Ex::new(amount.to_string(), true).using(&[Feature::ShiftByLiteral])
                } else {
                    let other = self.rng.pick(&Ty::INTS);
                    let e = self.int_expr(other, depth, true);
                    Ex::new(e.text.clone(), false)
                        .and(&e)
                        .using(&[Feature::ShiftByValue])
                }
            }
            _ => self.int_expr(ty, depth, false),
        }
    }

    /// A cast to `ty` of an unknown value of another or the same type.
    fn cast_to(&mut self, ty: Ty, depth: u32) -> Ex {
        let from = if self.rng.chance(1, 5) {
            Ty::Bool
        } else {
            self.rng.pick(&Ty::INTS)
        };
        let e = self.expr(from, depth, true);
        let feature = if from == Ty::Bool {
            Feature::CastBool
        } else {
            Feature::Cast
        };
        Ex::new(format!("({} as {})", e.text, ty.name()), false)
            .and(&e)
            .using(&[feature])
    }

    fn if_expr(&mut self, ty: Ty, depth: u32, unknown: bool) -> Ex {
        let cond = self.bool_expr(depth, false);
        let a = self.int_expr(ty, depth, unknown);
        let b = self.int_expr(ty, depth, false);
        Ex::new(
            format!("(if {} {{ {} }} else {{ {} }})", cond.text, a.text, b.text),
            false,
        )
        .and(&cond)
        .and(&a)
        .and(&b)
        .using(&[Feature::IfExpr])
    }

    /// A `match` on an unknown integer, with literal arms, some of them
    /// or-patterns, and a last arm that is `_` or a `let` binding.
    fn match_expr(&mut self, ty: Ty, depth: u32, unknown: bool) -> Ex {
        let scrutinee_ty = self.rng.pick(&Ty::INTS);
        let scrutinee = self.int_expr(scrutinee_ty, depth, true);
        let mut ex = Ex::new(String::new(), false)
            .and(&scrutinee)
            .using(&[Feature::Match]);
        let mut text = format!("(match {} {{ ", scrutinee.text);
        let mut seen: Vec<i128> = Vec::new();
        let mut first = true;
        for _ in 0..1 + self.rng.below(3) {
            let mut alternatives = Vec::new();
            for _ in 0..1 + usize::from(self.rng.chance(1, 3)) {
                let value = self.literal(scrutinee_ty);
                if !seen.contains(&value) {
                    seen.push(value);
                    alternatives.push(value.to_string());
                }
            }
            if alternatives.len() > 1 {
                ex = ex.using(&[Feature::MatchOr]);
            }
            if alternatives.is_empty() {
                continue;
            }
            let arm = self.int_expr(ty, depth, unknown && first);
            first = false;
            ex = ex.and(&arm);
            let _ = write!(text, "{} => {}, ", alternatives.join(" | "), arm.text);
        }
        if self.rng.chance(1, 2) {
            let arm = self.int_expr(ty, depth, unknown && first);
            ex = ex.and(&arm);
            let _ = write!(text, "_ => {}, ", arm.text);
        } else {
            let name = self.fresh("m");
            let mark = self.vars.len();
            self.vars.push(Var {
                name: name.clone(),
                ty: scrutinee_ty,
                mutable: false,
            });
            let arm = self.int_expr(ty, depth, unknown && first);
            self.vars.truncate(mark);
            ex = ex.and(&arm).using(&[Feature::MatchBind]);
            let _ = write!(
                text,
                "let {name}: {} => {}, ",
                scrutinee_ty.name(),
                arm.text
            );
        }
        text.push_str("})");
        ex.text = text;
        ex
    }

    /// A `bool` expression. With `unknown`, one that is not known at
    /// compile time.
    fn bool_expr(&mut self, depth: u32, unknown: bool) -> Ex {
        if depth == 0 {
            return self.leaf(Ty::Bool, unknown);
        }
        let d = depth - 1;
        let ex = match self.rng.below(8) {
            0..=3 => {
                let ty = self.rng.pick(&Ty::INTS);
                let cmp = self.rng.pick(&Cmp::ALL);
                let a = self.int_expr(ty, d, true);
                let b = self.int_expr(ty, d, false);
                Ex::new(format!("({} {} {})", a.text, cmp.symbol(), b.text), false)
                    .and(&a)
                    .and(&b)
                    .using(&[cmp.feature()])
            }
            4 => {
                let a = self.bool_expr(d, unknown);
                Ex::new(format!("(!{})", a.text), a.known)
                    .and(&a)
                    .using(&[Feature::NotBool])
            }
            5 => {
                let cmp = self.rng.pick(&[Cmp::Eq, Cmp::Ne]);
                let a = self.bool_expr(d, true);
                let b = self.bool_expr(d, false);
                Ex::new(format!("({} {} {})", a.text, cmp.symbol(), b.text), false)
                    .and(&a)
                    .and(&b)
                    .using(&[cmp.feature()])
            }
            _ => self.leaf(Ty::Bool, unknown),
        };
        if unknown && ex.known {
            self.unknown_leaf(Ty::Bool)
        } else {
            ex
        }
    }

    fn leaf(&mut self, ty: Ty, unknown: bool) -> Ex {
        if unknown {
            return self.unknown_leaf(ty);
        }
        if ty == Ty::Bool {
            if self.rng.chance(1, 2) {
                return self.unknown_leaf(ty);
            }
            let literal = if self.rng.chance(1, 2) {
                "true"
            } else {
                "false"
            };
            return Ex::new(literal, true);
        }
        match self.rng.below(3) {
            0 => self.unknown_leaf(ty),
            _ => {
                let value = self.literal(ty);
                Ex::new(literal_text(value), true)
            }
        }
    }

    /// A variable of type `ty`, or a cast of another one, or, for `bool`, a
    /// comparison: an expression that reads a variable.
    fn unknown_leaf(&mut self, ty: Ty) -> Ex {
        let same: Vec<Var> = self.vars.iter().filter(|v| v.ty == ty).cloned().collect();
        if !same.is_empty() && self.rng.chance(3, 4) {
            return Ex::new(same[self.rng.below(same.len())].name.clone(), false);
        }
        if ty == Ty::Bool {
            let from = self.rng.pick(&Ty::INTS);
            let cmp = self.rng.pick(&Cmp::ALL);
            let a = self.unknown_leaf(from);
            let value = self.literal(from);
            return Ex::new(
                format!("({} {} {})", a.text, cmp.symbol(), literal_text(value)),
                false,
            )
            .and(&a)
            .using(&[cmp.feature()]);
        }
        let any: Vec<Var> = self.vars.clone();
        let var = any[self.rng.below(any.len())].clone();
        if var.ty == ty {
            return Ex::new(var.name, false);
        }
        let feature = if var.ty == Ty::Bool {
            Feature::CastBool
        } else {
            Feature::Cast
        };
        Ex::new(format!("({} as {})", var.name, ty.name()), false).using(&[feature])
    }

    /// A value of `ty`, favoring the edges: zero, one, the extremes and
    /// small numbers.
    fn literal(&mut self, ty: Ty) -> i128 {
        let (min, max) = (ty.min(), ty.max());
        match self.rng.below(8) {
            0 => 0,
            1 => 1,
            2 => max,
            3 => min,
            4 => max - 1,
            5 => {
                let small = self.rng.below(24) as i128;
                if ty.signed() && self.rng.chance(1, 2) {
                    -small
                } else {
                    small
                }
            }
            _ => {
                let raw = (self.rng.next() as i128) & ((1i128 << ty.bits()) - 1);
                if ty.signed() && raw > max {
                    raw - (1i128 << ty.bits())
                } else {
                    raw
                }
            }
        }
    }
}

/// A literal as an expression: a negative one is parenthesized.
fn literal_text(value: i128) -> String {
    if value < 0 {
        format!("({value})")
    } else {
        value.to_string()
    }
}
