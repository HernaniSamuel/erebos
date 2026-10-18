//! Exhaustiveness and dead arms of a `match` (ADR 0037, 0040), by the
//! usefulness of patterns, after Maranget, "Warnings for pattern matching"
//! (Journal of Functional Programming, 2007).
//!
//! The patterns of a `match` are rows, one per arm, an or-pattern standing
//! for one row per alternative; each column is a position, the scrutinee
//! first, and a variant's payload positions take its place when it is
//! examined. A pattern is useful after some rows when it matches a value
//! that none of them matches: an alternative that is not useful after the
//! arms and alternatives before it is dead, and the values `_` matches
//! that no row matches are what a `match` misses.
//!
//! An integer position is examined through the values its patterns name
//! and one value they do not, never value by value, so a `u64` position
//! costs what its arms name.
//!
//! Values the rows do not tell apart are examined once: two values, or two
//! alternatives of an or-pattern, at one position are alike when the rows
//! that match each, and in `useful` the pattern left after each, are the
//! same patterns of the same arms, and then they get the same answer. An
//! or-pattern of values without payload, such as `1 | 2 | 3`, so costs
//! one alternative where the rows treat its values alike, instead of
//! multiplying the cost of the positions after it. Only values at the same
//! position are compared, so the worst case stays exponential (ADR 0043).

use std::cell::Cell;
use std::collections::{HashMap, HashSet};

use crate::hir::{EnumId, EnumTy, IntTy};

/// The type of a position, as far as patterns tell its values apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Column {
    Bool,
    Int(IntTy),
    Enum(EnumTy),
    /// A struct, an array, a pointer, a slice or `()`, whose values no
    /// pattern tells apart: only `_` and `let` match there (ADR 0040).
    Other,
}

/// A variant of an enum: its value, and the types of its payload values.
#[derive(Debug)]
pub struct Variant {
    pub value: u64,
    pub payload: Vec<Column>,
}

/// The variants of the enums whose values a `match` needs, in declaration
/// order (ADR 0038). Only those are ever examined value by value.
pub type Enums = HashMap<EnumId, Vec<Variant>>;

/// The state of the searches of one `match`: the variants of its enums,
/// and how many times `useful` and `missing` were called, which the tests
/// bound.
pub struct Search<'e> {
    enums: &'e Enums,
    calls: Cell<u64>,
}

impl<'e> Search<'e> {
    pub fn new(enums: &'e Enums) -> Self {
        Search {
            enums,
            calls: Cell::new(0),
        }
    }

    fn call(&self) {
        self.calls.set(self.calls.get() + 1);
    }

    /// How many times `useful` and `missing` were called so far.
    #[cfg(test)]
    fn calls(&self) -> u64 {
        self.calls.get()
    }
}

/// A pattern, as the checks see it: what it matches, not how it is
/// written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Pat {
    /// `_` or a `let`: every value.
    Wild,
    /// An integer, `bool` or enum value, as the bits of its type (`bool` is
    /// 0 or 1), with the patterns of its payload values if it is a variant
    /// that has a payload.
    Value(u64, Vec<Pat>),
    /// Any of the alternatives.
    Or(Vec<Pat>),
}

impl Pat {
    /// This pattern with `with` in place of the pattern at `path`, each
    /// step the index of a payload pattern of a variant.
    pub fn replaced(&self, path: &[usize], with: &Pat) -> Pat {
        let Some((&index, rest)) = path.split_first() else {
            return with.clone();
        };
        match self {
            Pat::Value(value, subs) => {
                let mut subs = subs.clone();
                subs[index] = subs[index].replaced(rest, with);
                Pat::Value(*value, subs)
            }
            _ => unreachable!("a path leads through payload patterns"),
        }
    }
}

/// Fills the payload positions of a variant that a `_` matches.
static WILD: Pat = Pat::Wild;

/// One pattern per column.
pub type Row<'p> = Vec<&'p Pat>;

/// Whether `pattern`, one pattern per column of `columns`, matches a value
/// that no row matches. The values of an enum include those that are no
/// variant, as dead arms count them (ADR 0036, 0037).
pub fn useful<'p>(
    search: &Search,
    columns: &[Column],
    rows: &[Row<'p>],
    pattern: &[&'p Pat],
) -> bool {
    search.call();
    // A row of `_` alone leaves nothing; stopping here keeps the search
    // from examining the values of every later column.
    if rows.iter().any(|row| all_wild(row)) {
        return false;
    }
    let Some((&head, tail)) = pattern.split_first() else {
        return rows.is_empty();
    };
    let column = columns[0];
    match head {
        Pat::Or(alternatives) => {
            let rows = expand(rows);
            // The searches of the alternatives examined, none useful: an
            // alternative alike to one of them is not useful either.
            let mut examined: Vec<Next> = Vec::new();
            for alternative in alternatives {
                let Pat::Value(value, subs) = alternative else {
                    if useful(search, columns, &rows, &prepend(alternative, tail)) {
                        return true;
                    }
                    continue;
                };
                let next = Next::new(search, column, &columns[1..], &rows, *value, subs, tail);
                if examined.iter().any(|earlier| earlier.alike(&next)) {
                    continue;
                }
                if useful(search, &next.columns, &next.rows, &next.pattern) {
                    return true;
                }
                examined.push(next);
            }
            false
        }
        Pat::Value(value, subs) => {
            let next = Next::new(
                search,
                column,
                &columns[1..],
                &expand(rows),
                *value,
                subs,
                tail,
            );
            useful(search, &next.columns, &next.rows, &next.pattern)
        }
        Pat::Wild => {
            let rows = expand(rows);
            let named = named(&rows);
            if named.len() as u128 == count(column) {
                // Every value is named: `_` is useful if it is for one of
                // them, and a value alike to one already examined is not.
                let mut examined: Vec<Next> = Vec::new();
                for value in named {
                    let next = Next::wild(search, column, &columns[1..], &rows, value, tail);
                    if examined.iter().any(|earlier| earlier.alike(&next)) {
                        continue;
                    }
                    if useful(search, &next.columns, &next.rows, &next.pattern) {
                        return true;
                    }
                    examined.push(next);
                }
                false
            } else {
                // A value no row names is matched only by rows with `_`.
                useful(search, &columns[1..], &default(&rows), tail)
            }
        }
    }
}

/// The first `limit` values that no row matches, the values of an enum
/// being only its variants (ADR 0036): each as a pattern per column,
/// without or-patterns, in the order ADR 0040 finds them. The search stops
/// once it has found `limit` (ADR 0043), since their number can grow
/// exponentially with the patterns. With `examine`, a first column of type
/// `bool` or enum lists its values even when no row remains, as for a
/// `match` without arms (ADR 0037).
pub fn missing<'p>(
    search: &Search,
    columns: &[Column],
    rows: &[Row<'p>],
    examine: bool,
    limit: usize,
) -> Vec<Vec<Pat>> {
    search.call();
    if limit == 0 {
        return Vec::new();
    }
    let Some((&column, rest)) = columns.split_first() else {
        return if rows.is_empty() {
            vec![Vec::new()]
        } else {
            Vec::new()
        };
    };
    let rows = expand(rows);
    // A row of `_` alone matches every value left: no witness remains, and
    // stopping here keeps the search from examining the values of every
    // later column.
    if rows.iter().any(|row| all_wild(row)) {
        return Vec::new();
    }
    let listed = examine && rows.is_empty() && matches!(column, Column::Bool | Column::Enum(_));
    if rows.iter().all(|row| matches!(row[0], Pat::Wild)) && !listed {
        return missing(search, rest, &default(&rows), false, limit)
            .into_iter()
            .map(|witness| prepend(Pat::Wild, &witness))
            .collect();
    }
    let values: Vec<u64> = match column {
        Column::Bool => vec![1, 0],
        Column::Enum(e) => variants(search.enums, e).iter().map(|v| v.value).collect(),
        Column::Int(int) => {
            let mut values = named(&rows);
            values.extend(unnamed(int, &values));
            values
        }
        Column::Other => unreachable!("only `_` and `let` match where no pattern compares"),
    };
    let mut witnesses = Vec::new();
    // The searches of the values examined, each with the witnesses it found
    // after its value: a value alike to one of them has the same ones.
    let mut examined: Vec<(Next, Vec<Vec<Pat>>)> = Vec::new();
    for value in values {
        let wanted = limit - witnesses.len();
        if wanted == 0 {
            break;
        }
        // No pattern is searched for: the one left is `_` at each payload
        // position, the same for values of the same payload.
        let next = Next::wild(search, column, rest, &rows, value, &[]);
        let arity = next.columns.len() - rest.len();
        let index = match examined
            .iter()
            .position(|(earlier, _)| earlier.alike(&next))
        {
            Some(index) => index,
            None => {
                let found = missing(search, &next.columns, &next.rows, false, wanted);
                examined.push((next, found));
                examined.len() - 1
            }
        };
        // The earlier value was given at least as many to find: the first
        // `wanted` of its witnesses are those this value would find.
        for subs in examined[index].1.iter().take(wanted) {
            let mut subs = subs.clone();
            let after = subs.split_off(arity);
            witnesses.push(prepend(Pat::Value(value, subs), &after));
        }
    }
    witnesses
}

/// The search one position further, once a value is taken at the first
/// column: the columns that follow, the rows that match the value with its
/// payload positions in place of the first column, and, in `useful`, the
/// pattern left.
struct Next<'p> {
    columns: Vec<Column>,
    rows: Vec<Row<'p>>,
    pattern: Row<'p>,
}

impl<'p> Next<'p> {
    /// After `value`, with the payload patterns `subs`, at a column of type
    /// `column` followed by the columns `rest`: `rows` are expanded, and the
    /// pattern is that value followed by `tail`.
    fn new(
        search: &Search,
        column: Column,
        rest: &[Column],
        rows: &[Row<'p>],
        value: u64,
        subs: &'p [Pat],
        tail: &[&'p Pat],
    ) -> Self {
        let payload = payload(search.enums, column, value);
        Next {
            columns: concat(payload, rest),
            rows: specialize(rows, value, payload.len()),
            pattern: subs.iter().chain(tail.iter().copied()).collect(),
        }
    }

    /// As `new`, for `value` taken by a `_`, which leaves `_` at each of
    /// its payload positions.
    fn wild(
        search: &Search,
        column: Column,
        rest: &[Column],
        rows: &[Row<'p>],
        value: u64,
        tail: &[&'p Pat],
    ) -> Self {
        let payload = payload(search.enums, column, value);
        Next {
            columns: concat(payload, rest),
            rows: specialize(rows, value, payload.len()),
            pattern: std::iter::repeat_n(&WILD, payload.len())
                .chain(tail.iter().copied())
                .collect(),
        }
    }

    /// Whether the search goes on alike after both values: the same
    /// columns, and the same rows and pattern left, pattern by pattern, as
    /// the same patterns of the same arms.
    fn alike(&self, other: &Next) -> bool {
        self.rows.len() == other.rows.len()
            && self.rows.iter().zip(&other.rows).all(|(a, b)| same(a, b))
            && same(&self.pattern, &other.pattern)
            && self.columns == other.columns
    }
}

/// Whether two rows hold the same patterns, by address.
fn same(a: &[&Pat], b: &[&Pat]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| std::ptr::eq(*x, *y))
}

fn variants(enums: &Enums, e: EnumTy) -> &[Variant] {
    enums
        .get(&e.id)
        .expect("an enum examined value by value is one whose values the `match` needs")
}

/// The types of the payload of `value` at a position of type `column`:
/// none, except for a variant that has a payload. A value that is no
/// variant has none.
fn payload(enums: &Enums, column: Column, value: u64) -> &[Column] {
    match column {
        Column::Enum(e) if e.payloads => variants(enums, e)
            .iter()
            .find(|variant| variant.value == value)
            .map_or(&[], |variant| &variant.payload),
        _ => &[],
    }
}

/// How many values a position of type `column` has, an enum's being all
/// those of its integer type. No pattern names a value where patterns do
/// not compare, so that `_` there is never compared value by value.
fn count(column: Column) -> u128 {
    match column {
        Column::Bool => 2,
        Column::Int(int) | Column::Enum(EnumTy { int, .. }) => u128::from(int.mask()) + 1,
        Column::Other => u128::MAX,
    }
}

/// Whether every pattern of `row` is `_`.
fn all_wild(row: &[&Pat]) -> bool {
    row.iter().all(|pattern| matches!(pattern, Pat::Wild))
}

/// The rows, with each one whose first pattern is an or-pattern replaced by
/// one row per alternative, in order.
fn expand<'p>(rows: &[Row<'p>]) -> Vec<Row<'p>> {
    fn push<'p>(out: &mut Vec<Row<'p>>, head: &'p Pat, tail: &[&'p Pat]) {
        match head {
            Pat::Or(alternatives) => {
                for alternative in alternatives {
                    push(out, alternative, tail);
                }
            }
            _ => out.push(prepend(head, tail)),
        }
    }
    let mut out = Vec::new();
    for row in rows {
        push(&mut out, row[0], &row[1..]);
    }
    out
}

/// The values the first patterns of `rows`, expanded, name, each once, in
/// the order they first name it.
fn named(rows: &[Row]) -> Vec<u64> {
    let mut seen = HashSet::new();
    rows.iter()
        .filter_map(|row| match row[0] {
            Pat::Value(value, _) => seen.insert(*value).then_some(*value),
            _ => None,
        })
        .collect()
}

/// The value of `int` closest to zero that `named` does not hold, the
/// non-negative one first; none if `named` holds every value.
fn unnamed(int: IntTy, named: &[u64]) -> Option<u64> {
    if named.len() as u128 > u128::from(int.mask()) {
        return None;
    }
    let named: HashSet<u64> = named.iter().copied().collect();
    let signed = int.is_signed();
    let mask = int.mask();
    // 0, 1, -1, 2, -2, ..., max, -max, and -(max + 1) for signed types.
    let negative = move |k: u64| (signed && k > 0).then(|| k.wrapping_neg() & mask);
    (0..=int.max())
        .flat_map(move |k| std::iter::once(k).chain(negative(k)))
        .chain(signed.then(|| (int.max() + 1).wrapping_neg() & mask))
        .find(|value| !named.contains(value))
}

/// The rows that match `value` at the first column, expanded, with its
/// `arity` payload positions in place of the first column.
fn specialize<'p>(rows: &[Row<'p>], value: u64, arity: usize) -> Vec<Row<'p>> {
    rows.iter()
        .filter_map(|row| {
            let tail = row[1..].iter().copied();
            match row[0] {
                Pat::Value(named, subs) if *named == value => {
                    Some(subs.iter().chain(tail).collect())
                }
                Pat::Value(..) => None,
                Pat::Wild => Some(std::iter::repeat_n(&WILD, arity).chain(tail).collect()),
                Pat::Or(_) => unreachable!("the rows are expanded"),
            }
        })
        .collect()
}

/// The rows, expanded, whose first pattern is `_`, without it.
fn default<'p>(rows: &[Row<'p>]) -> Vec<Row<'p>> {
    rows.iter()
        .filter(|row| matches!(row[0], Pat::Wild))
        .map(|row| row[1..].to_vec())
        .collect()
}

fn prepend<T: Clone>(head: T, tail: &[T]) -> Vec<T> {
    let mut out = Vec::with_capacity(tail.len() + 1);
    out.push(head);
    out.extend_from_slice(tail);
    out
}

fn concat(a: &[Column], b: &[Column]) -> Vec<Column> {
    [a, b].concat()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn value(value: u64) -> Pat {
        Pat::Value(value, Vec::new())
    }

    fn enum_ty(int: IntTy) -> EnumTy {
        EnumTy {
            id: EnumId(0),
            name: "E",
            int,
            payloads: false,
        }
    }

    /// The variants of `E`, without payloads, of these values.
    fn enums(values: &[u64]) -> Enums {
        let variants = values
            .iter()
            .map(|&value| Variant {
                value,
                payload: Vec::new(),
            })
            .collect();
        Enums::from([(EnumId(0), variants)])
    }

    /// One row per pattern, at one column.
    fn rows(patterns: &[Pat]) -> Vec<Row<'_>> {
        patterns.iter().map(|pattern| vec![pattern]).collect()
    }

    fn useful1(enums: &Enums, column: Column, earlier: &[Pat], pattern: &Pat) -> bool {
        useful(&Search::new(enums), &[column], &rows(earlier), &[pattern])
    }

    /// The witnesses of `arms` at one column, up to `limit`.
    fn missing_up_to(enums: &Enums, column: Column, arms: &[Pat], limit: usize) -> Vec<Vec<Pat>> {
        missing(
            &Search::new(enums),
            &[column],
            &rows(arms),
            arms.is_empty(),
            limit,
        )
    }

    /// Every witness of `arms` at one column: no test here has as many as
    /// 100.
    fn missing1(enums: &Enums, column: Column, arms: &[Pat]) -> Vec<Vec<Pat>> {
        missing_up_to(enums, column, arms, 100)
    }

    #[test]
    fn integer_values_and_wildcards() {
        let none = Enums::new();
        let u8_ = Column::Int(IntTy::U8);
        assert!(useful1(&none, u8_, &[], &value(3)));
        assert!(!useful1(&none, u8_, &[value(3)], &value(3)));
        assert!(useful1(&none, u8_, &[value(3)], &value(4)));
        assert!(useful1(&none, u8_, &[value(3)], &Pat::Wild));
        assert!(!useful1(&none, u8_, &[Pat::Wild], &value(3)));
        assert!(!useful1(&none, u8_, &[Pat::Wild], &Pat::Wild));
        // An or-pattern is useful if one of its alternatives is.
        let or = Pat::Or(vec![value(3), value(4)]);
        assert!(useful1(&none, u8_, &[value(3)], &or));
        assert!(!useful1(&none, u8_, &[value(4), value(3)], &or));
        // An earlier or-pattern covers each of its alternatives.
        assert!(!useful1(&none, u8_, std::slice::from_ref(&or), &value(4)));
        assert!(!missing1(&none, u8_, &[value(3)]).is_empty());
        assert!(missing1(&none, u8_, &[value(3), Pat::Wild]).is_empty());
    }

    #[test]
    fn every_u8_value_is_exhaustive() {
        let none = Enums::new();
        let u8_ = Column::Int(IntTy::U8);
        let all: Vec<Pat> = (0..=255).map(value).collect();
        assert!(missing1(&none, u8_, &all).is_empty());
        // A `_` after them is dead.
        assert!(!useful1(&none, u8_, &all, &Pat::Wild));
        // One value fewer leaves exactly that one.
        let most: Vec<Pat> = (0..=255).filter(|&v| v != 200).map(value).collect();
        assert_eq!(missing1(&none, u8_, &most), vec![vec![value(200)]]);
        assert!(useful1(&none, u8_, &most, &Pat::Wild));
        assert!(!useful1(&none, u8_, &most, &value(7)));
    }

    #[test]
    fn u64_values_near_the_max_are_not_enumerated() {
        let none = Enums::new();
        let u64_ = Column::Int(IntTy::U64);
        let near: Vec<Pat> = (0..4).map(|k| value(u64::MAX - k)).collect();
        assert!(!useful1(&none, u64_, &near, &value(u64::MAX)));
        assert!(useful1(&none, u64_, &near, &value(u64::MAX - 4)));
        assert!(useful1(&none, u64_, &near, &Pat::Wild));
        // The missing values are those named, all covered, then the one
        // closest to zero.
        assert_eq!(missing1(&none, u64_, &near), vec![vec![value(0)]]);
        let zero = [value(0), value(1), value(u64::MAX)];
        assert_eq!(missing1(&none, u64_, &zero), vec![vec![value(2)]]);
        let mut with_wild = near.clone();
        with_wild.push(Pat::Wild);
        assert!(missing1(&none, u64_, &with_wild).is_empty());
    }

    #[test]
    fn signed_values_closest_to_zero() {
        let none = Enums::new();
        let i8_ = Column::Int(IntTy::I8);
        let minus = |k: u64| k.wrapping_neg() & IntTy::I8.mask();
        assert_eq!(missing1(&none, i8_, &[value(0)]), vec![vec![value(1)]]);
        assert_eq!(
            missing1(&none, i8_, &[value(0), value(1)]),
            vec![vec![value(minus(1))]]
        );
        // Every value but -128.
        let most: Vec<Pat> = (0..=127).chain((1..=127).map(minus)).map(value).collect();
        assert_eq!(missing1(&none, i8_, &most), vec![vec![value(minus(128))]]);
        let i64_ = Column::Int(IntTy::I64);
        assert_eq!(
            missing1(&none, i64_, &[value(0), value(1), value(u64::MAX)]),
            vec![vec![value(2)]]
        );
    }

    #[test]
    fn bool_values() {
        let none = Enums::new();
        let (t, f) = (value(1), value(0));
        assert_eq!(
            missing1(&none, Column::Bool, &[]),
            vec![vec![t.clone()], vec![f.clone()]]
        );
        assert_eq!(
            missing1(&none, Column::Bool, std::slice::from_ref(&t)),
            vec![vec![f.clone()]]
        );
        assert!(missing1(&none, Column::Bool, &[f.clone(), t.clone()]).is_empty());
        // `_` after `true` and `false` is dead.
        assert!(!useful1(
            &none,
            Column::Bool,
            &[t.clone(), f.clone()],
            &Pat::Wild
        ));
        assert!(useful1(
            &none,
            Column::Bool,
            std::slice::from_ref(&t),
            &Pat::Wild
        ));
        // With arms, only `_` remains for a `bool` they do not examine.
        assert!(missing1(&none, Column::Bool, &[Pat::Wild]).is_empty());
    }

    #[test]
    fn enum_values_that_are_no_variant() {
        // Variants 0, 1 and 5 of a `u8`.
        let e = Column::Enum(enum_ty(IntTy::U8));
        let enums = enums(&[0, 1, 5]);
        let variants = [value(0), value(1), value(5)];
        // Every variant is exhaustive, yet `_` after them is not dead.
        assert!(missing1(&enums, e, &variants).is_empty());
        assert!(useful1(&enums, e, &variants, &Pat::Wild));
        // Missing variants in declaration order, whatever the arms' order.
        assert_eq!(
            missing1(&enums, e, &[value(1)]),
            vec![vec![value(0)], vec![value(5)]]
        );
        assert_eq!(
            missing1(&enums, e, &[]),
            vec![vec![value(0)], vec![value(1)], vec![value(5)]]
        );
        // A constant holding a value that is no variant covers nothing
        // required, and is not dead.
        assert!(useful1(&enums, e, &variants, &value(9)));
        assert_eq!(
            missing1(&enums, e, &[value(9), value(5)]),
            vec![vec![value(0)], vec![value(1)]]
        );
        // `_` alone needs no variant.
        assert!(missing1(&Enums::new(), e, &[Pat::Wild]).is_empty());
    }

    #[test]
    fn enum_with_every_value_a_variant() {
        // 256 variants on a `u8`: no value is left for `_`.
        let e = Column::Enum(enum_ty(IntTy::U8));
        let values: Vec<u64> = (0..=255).collect();
        let enums = enums(&values);
        let all: Vec<Pat> = values.iter().map(|&v| value(v)).collect();
        assert!(missing1(&enums, e, &all).is_empty());
        assert!(!useful1(&enums, e, &all, &Pat::Wild));
    }

    #[test]
    fn variant_payloads_take_the_variants_place() {
        // `enum P: u8 { A(bool) = 0, B = 1 }`.
        let p = EnumTy {
            payloads: true,
            ..enum_ty(IntTy::U8)
        };
        let enums = Enums::from([(
            EnumId(0),
            vec![
                Variant {
                    value: 0,
                    payload: vec![Column::Bool],
                },
                Variant {
                    value: 1,
                    payload: Vec::new(),
                },
            ],
        )]);
        let column = Column::Enum(p);
        let a = |b: Pat| Pat::Value(0, vec![b]);
        let arms = [a(value(1)), value(1)];
        assert_eq!(missing1(&enums, column, &arms), vec![vec![a(value(0))]]);
        assert!(useful1(&enums, column, &arms, &a(Pat::Wild)));
        assert!(!useful1(&enums, column, &arms, &a(value(1))));
        let all = [a(value(1)), a(value(0)), value(1)];
        assert!(missing1(&enums, column, &all).is_empty());
        assert!(!useful1(&enums, column, &all, &a(Pat::Wild)));
    }

    /// `enum Mode: u8 { Off = 0, Run = 1 }` as `EnumId(1)`, and
    /// `enum Msg: u8 { Nop = 0, Led(bool) = 1, Write(u8, u16) = 2,
    /// Wide(u32) = 3, SetMode(Mode) = 4 }` as `EnumId(0)` (ADR 0040).
    fn msg() -> (Column, Column, Enums) {
        let mode = EnumTy {
            id: EnumId(1),
            name: "Mode",
            ..enum_ty(IntTy::U8)
        };
        let msg = EnumTy {
            payloads: true,
            ..enum_ty(IntTy::U8)
        };
        let variant = |value: u64, payload: Vec<Column>| Variant { value, payload };
        let enums = Enums::from([
            (
                EnumId(0),
                vec![
                    variant(0, Vec::new()),
                    variant(1, vec![Column::Bool]),
                    variant(2, vec![Column::Int(IntTy::U8), Column::Int(IntTy::U16)]),
                    variant(3, vec![Column::Int(IntTy::U32)]),
                    variant(4, vec![Column::Enum(mode)]),
                ],
            ),
            (
                EnumId(1),
                vec![variant(0, Vec::new()), variant(1, Vec::new())],
            ),
        ]);
        (Column::Enum(msg), Column::Enum(mode), enums)
    }

    #[test]
    fn nested_witnesses_in_written_order() {
        let (msg, _, enums) = msg();
        let v = |value: u64, subs: Vec<Pat>| Pat::Value(value, subs);
        // `Msg::Nop`, `Msg::Led(true)`, `Msg::Write(0, 5)`.
        let arms = [
            v(0, vec![]),
            v(1, vec![value(1)]),
            v(2, vec![value(0), value(5)]),
        ];
        assert_eq!(
            missing1(&enums, msg, &arms),
            vec![
                vec![v(1, vec![value(0)])],
                vec![v(2, vec![value(0), value(0)])],
                vec![v(2, vec![value(1), Pat::Wild])],
                vec![v(3, vec![Pat::Wild])],
                vec![v(4, vec![Pat::Wild])],
            ]
        );
        // Without arms, every variant, with `_` for each payload value.
        assert_eq!(missing1(&enums, msg, &[]).len(), 5);
        assert_eq!(
            missing1(&enums, msg, &[])[2],
            vec![v(2, vec![Pat::Wild, Pat::Wild])]
        );
    }

    #[test]
    fn the_search_stops_at_the_limit() {
        let (msg, _, enums) = msg();
        let v = |value: u64, subs: Vec<Pat>| Pat::Value(value, subs);
        // The five witnesses of `nested_witnesses_in_written_order`.
        let arms = [
            v(0, vec![]),
            v(1, vec![value(1)]),
            v(2, vec![value(0), value(5)]),
        ];
        let all = missing_up_to(&enums, msg, &arms, 100);
        assert_eq!(all.len(), 5);
        // A limit keeps the order: the witnesses are the first ones.
        assert_eq!(missing_up_to(&enums, msg, &arms, 4), all[..4]);
        assert_eq!(missing_up_to(&enums, msg, &arms, 1), all[..1]);
        assert_eq!(missing_up_to(&enums, msg, &arms, 5), all);
        assert!(missing_up_to(&enums, msg, &arms, 0).is_empty());
        // Without arms, the variants listed are the first ones too.
        assert_eq!(
            missing_up_to(&enums, msg, &[], 4),
            missing_up_to(&enums, msg, &[], 100)[..4]
        );
    }

    #[test]
    fn exponentially_many_witnesses_are_not_enumerated() {
        // pairs(40) (ADR 0042): `enum P: u8 { A(bool, ..., bool) = 0 }` with
        // 80 `bool`s, and one arm per `i < 40` with `true` at `i` and
        // `i + 40` and `_` elsewhere. It has 2^40 witnesses, one per choice
        // of the first 40 positions: without the limit, the search would
        // write every one.
        const N: usize = 40;
        let p = EnumTy {
            payloads: true,
            ..enum_ty(IntTy::U8)
        };
        let enums = Enums::from([(
            EnumId(0),
            vec![Variant {
                value: 0,
                payload: vec![Column::Bool; 2 * N],
            }],
        )]);
        let column = Column::Enum(p);
        let arms: Vec<Pat> = (0..N)
            .map(|i| {
                let subs = (0..2 * N)
                    .map(|j| {
                        if j == i || j == i + N {
                            value(1)
                        } else {
                            Pat::Wild
                        }
                    })
                    .collect();
                Pat::Value(0, subs)
            })
            .collect();
        let witnesses = missing_up_to(&enums, column, &arms, 4);
        assert_eq!(witnesses.len(), 4);
        // `true` is examined before `false` (ADR 0040), and leaves every arm
        // until position `N`, where each `false` leaves one fewer:
        // `P::A(true, ..., true, false, ..., false)`.
        let first: Vec<Pat> = std::iter::repeat_n(value(1), N)
            .chain(std::iter::repeat_n(value(0), N))
            .collect();
        assert_eq!(witnesses[0], vec![Pat::Value(0, first)]);
    }

    #[test]
    fn nested_values_that_are_no_variant() {
        let (msg, _, enums) = msg();
        let v = |value: u64, subs: Vec<Pat>| Pat::Value(value, subs);
        let set_mode = |mode: Pat| v(4, vec![mode]);
        let earlier = [
            v(0, vec![]),
            v(1, vec![Pat::Wild]),
            v(2, vec![Pat::Wild, Pat::Wild]),
            v(3, vec![Pat::Wild]),
            set_mode(value(0)),
            set_mode(value(1)),
        ];
        // Exhaustive, yet `Msg::SetMode(_)` and `_` are not dead.
        assert!(missing1(&enums, msg, &earlier).is_empty());
        assert!(useful1(&enums, msg, &earlier, &set_mode(Pat::Wild)));
        assert!(useful1(&enums, msg, &earlier, &Pat::Wild));
        // A dead alternative inside a payload: `Msg::Led(true | true)`.
        let led = |b: u64| v(1, vec![value(b)]);
        assert!(!useful1(&enums, msg, &[led(1)], &led(1)));
        assert!(useful1(
            &enums,
            msg,
            &[led(1)],
            &v(1, vec![Pat::Or(vec![value(1), value(0)])])
        ));
        // A whole arm covered by two earlier ones.
        assert!(!useful1(
            &enums,
            msg,
            &[led(1), led(0)],
            &v(1, vec![Pat::Wild])
        ));
    }

    #[test]
    fn positions_no_pattern_compares() {
        // `enum B: u8 { S(Pair, u8) = 0 }`, where only `_` and `let` match
        // a `Pair`.
        let b = EnumTy {
            payloads: true,
            ..enum_ty(IntTy::U8)
        };
        let enums = Enums::from([(
            EnumId(0),
            vec![Variant {
                value: 0,
                payload: vec![Column::Other, Column::Int(IntTy::U8)],
            }],
        )]);
        let s = |sub: Pat| Pat::Value(0, vec![Pat::Wild, sub]);
        let column = Column::Enum(b);
        assert_eq!(
            missing1(&enums, column, &[s(value(3))]),
            vec![vec![Pat::Value(0, vec![Pat::Wild, value(0)])]]
        );
        assert!(missing1(&enums, column, &[s(value(3)), s(Pat::Wild)]).is_empty());
        assert!(!useful1(&enums, column, &[s(Pat::Wild)], &s(value(1))));
    }

    #[test]
    fn replaced_takes_a_path_through_payloads() {
        let or = Pat::Or(vec![value(1), value(2)]);
        let whole = Pat::Value(0, vec![Pat::Wild, Pat::Value(5, vec![or])]);
        assert_eq!(
            whole.replaced(&[1, 0], &value(2)),
            Pat::Value(0, vec![Pat::Wild, Pat::Value(5, vec![value(2)])])
        );
        assert_eq!(whole.replaced(&[], &Pat::Wild), Pat::Wild);
    }

    #[test]
    fn a_row_of_wildcards_stops_the_search() {
        // `enum P: u8 { A(bool, ..., bool) = 0 }` with 64 `bool`s, and one
        // arm per position with `true` there and `_` elsewhere: without
        // stopping at a row of `_`, both searches would examine 2^64
        // combinations.
        const N: usize = 64;
        let p = EnumTy {
            payloads: true,
            ..enum_ty(IntTy::U8)
        };
        let enums = Enums::from([(
            EnumId(0),
            vec![Variant {
                value: 0,
                payload: vec![Column::Bool; N],
            }],
        )]);
        let column = Column::Enum(p);
        let arms: Vec<Pat> = (0..N)
            .map(|i| {
                let subs = (0..N)
                    .map(|j| if i == j { value(1) } else { Pat::Wild })
                    .collect();
                Pat::Value(0, subs)
            })
            .collect();
        // Only `P::A(false, ..., false)` is missing.
        assert_eq!(
            missing1(&enums, column, &arms),
            vec![vec![Pat::Value(0, vec![value(0); N])]]
        );
        assert!(useful1(&enums, column, &arms, &Pat::Wild));
        let mut all = arms.clone();
        all.push(Pat::Value(0, vec![value(0); N]));
        assert!(missing1(&enums, column, &all).is_empty());
        // `P::A(_, ..., _)` after them is dead; `_` is not, for the tags
        // that are no variant.
        assert!(!useful1(
            &enums,
            column,
            &all,
            &Pat::Value(0, vec![Pat::Wild; N])
        ));
        assert!(useful1(&enums, column, &all, &Pat::Wild));
    }

    /// ors(k) (ADR 0042), with the positions of `last` after its `k` `u8`
    /// positions: `enum Q: u8 { A(u8, ..., u8, last...) = 0 }`.
    fn ors(k: usize, last: &[Column]) -> (Column, Enums) {
        let q = EnumTy {
            payloads: true,
            ..enum_ty(IntTy::U8)
        };
        let payload = std::iter::repeat_n(Column::Int(IntTy::U8), k)
            .chain(last.iter().copied())
            .collect();
        let enums = Enums::from([(EnumId(0), vec![Variant { value: 0, payload }])]);
        (Column::Enum(q), enums)
    }

    /// `1 | 2 | 3`.
    fn one_two_three() -> Pat {
        Pat::Or(vec![value(1), value(2), value(3)])
    }

    #[test]
    fn alike_alternatives_are_examined_once() {
        // ors(40), with `1 | 2 | 3` at each position: examining every
        // alternative of each, as M2.4b did, takes 3^40 calls.
        const K: usize = 40;
        let (column, enums) = ors(K, &[]);
        let arm = Pat::Value(0, vec![one_two_three(); K]);
        let useful_within = |earlier: &[Pat], pattern: &Pat| {
            let search = Search::new(&enums);
            let useful = useful(&search, &[column], &rows(earlier), &[pattern]);
            assert!(search.calls() <= 10 * K as u64, "{} calls", search.calls());
            useful
        };
        // The same arm written again, as another pattern, is not useful.
        let again = arm.clone();
        assert!(!useful_within(std::slice::from_ref(&arm), &again));
        // With `4` in place of one position's or-pattern, it is.
        let mut four = vec![one_two_three(); K];
        four[K / 2] = value(4);
        assert!(useful_within(
            std::slice::from_ref(&arm),
            &Pat::Value(0, four)
        ));
        // The alternative `3` at the last position, in place of its
        // or-pattern, as dead alternatives are tried.
        let mut three = vec![one_two_three(); K];
        three[K - 1] = value(3);
        assert!(!useful_within(
            std::slice::from_ref(&arm),
            &Pat::Value(0, three)
        ));
    }

    #[test]
    fn alike_named_values_are_examined_once_for_a_wildcard() {
        // `enum B: u8 { A(bool, ..., bool) = 0 }` with 40 positions, and
        // `B::A(_, ..., _)` after `B::A(true | false, ..., true | false)`:
        // at each position `true` and `false` are both named and alike.
        // Examining both, as M2.4b did, takes 2^40 calls.
        const K: usize = 40;
        let b = EnumTy {
            payloads: true,
            ..enum_ty(IntTy::U8)
        };
        let enums = Enums::from([(
            EnumId(0),
            vec![Variant {
                value: 0,
                payload: vec![Column::Bool; K],
            }],
        )]);
        let column = Column::Enum(b);
        let either = Pat::Or(vec![value(1), value(0)]);
        let arm = Pat::Value(0, vec![either; K]);
        let search = Search::new(&enums);
        let wild = Pat::Value(0, vec![Pat::Wild; K]);
        assert!(!useful(
            &search,
            &[column],
            &rows(std::slice::from_ref(&arm)),
            &[&wild]
        ));
        assert!(search.calls() <= 10 * K as u64, "{} calls", search.calls());
    }

    #[test]
    fn alike_values_are_examined_once() {
        // ors(40) with a last `bool` position, and the arms
        // `A(1 | 2 | 3, ..., 1 | 2 | 3, true)`, `A(_, ..., _, false)` and
        // `A(_, ..., _, true)`: no row of `_` alone stops the search early.
        // At each depth the values `1`, `2` and `3` are alike, and the value
        // no arm names walks the positions left, about K^2 / 2 calls in all;
        // M2.4b examined 3^40 combinations.
        const K: usize = 40;
        let bound = 2 * (K * K) as u64;
        let (column, enums) = ors(K, &[Column::Bool]);
        let arm = |each: Pat, last: u64| {
            let subs = std::iter::repeat_n(each, K)
                .chain(std::iter::once(value(last)))
                .collect();
            Pat::Value(0, subs)
        };
        let arms = [
            arm(one_two_three(), 1),
            arm(Pat::Wild, 0),
            arm(Pat::Wild, 1),
        ];
        let search = Search::new(&enums);
        assert!(missing(&search, &[column], &rows(&arms), false, 4).is_empty());
        assert!(search.calls() <= bound, "{} calls", search.calls());
        // None of them is dead.
        for (i, pattern) in arms.iter().enumerate() {
            let search = Search::new(&enums);
            assert!(useful(&search, &[column], &rows(&arms[..i]), &[pattern]));
            assert!(search.calls() <= bound, "{} calls", search.calls());
        }
    }

    #[test]
    fn alike_values_share_their_witnesses_in_order() {
        // `enum Q: u8 { A(u8, u8) = 0 }`.
        let (column, enums) = ors(2, &[]);
        let a = |x: Pat, y: Pat| Pat::Value(0, vec![x, y]);
        // After `1` only the first arm remains, after `2` both: `1` and `2`
        // are not alike, and each is examined.
        let arms = [
            a(Pat::Or(vec![value(1), value(2)]), value(5)),
            a(value(2), value(6)),
        ];
        assert_eq!(
            missing1(&enums, column, &arms),
            vec![
                vec![a(value(1), value(0))],
                vec![a(value(2), value(0))],
                vec![a(value(0), Pat::Wild)],
            ]
        );
        // `1`, `2` and `3` are alike: the witnesses of `1` are written for
        // each, in place, before the value no arm names, and the limit
        // counts them.
        let arms = [a(one_two_three(), value(5))];
        let all = vec![
            vec![a(value(1), value(0))],
            vec![a(value(2), value(0))],
            vec![a(value(3), value(0))],
            vec![a(value(0), Pat::Wild)],
        ];
        assert_eq!(missing1(&enums, column, &arms), all);
        assert_eq!(missing_up_to(&enums, column, &arms, 2), all[..2]);
    }
}
