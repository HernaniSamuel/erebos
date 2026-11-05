//! The names of generated items (ADR 0071): an SVD name converted in steps,
//! and the scopes that keep two items from meeting.

use std::collections::HashMap;

use crate::span::Span;
use crate::syntax::token::TokenKind;

/// How a name is cased, by the kind of item (the table of ADR 0071).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Case {
    /// A peripheral keeps its case.
    Keep,
    /// Registers, clusters, fields, modules and groups.
    Lower,
    /// Peripheral types, cluster types, structs, enums and variants.
    Pascal,
}

/// Which index step 1 puts in a repeated name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Index<'a> {
    /// The item stands for the whole element: `[%s]` and `%s` are removed.
    Whole,
    /// One element of the repeated one: `[%s]` and `%s` are this index.
    Element(&'a str),
}

/// A converted name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Converted {
    pub name: String,
    /// Whether step 2 (the characters) or step 4 (a leading digit) changed
    /// what the SVD wrote: the item is then reported as renamed.
    pub changed: bool,
}

/// Steps 1 to 5 of ADR 0071 on the SVD name `name`; `None` when nothing is
/// left of it after step 2 (`svd::invalid-name`). Step 6, names that meet,
/// is a [`Scope`]'s.
pub(super) fn convert(name: &str, index: Index, case: Case) -> Option<Converted> {
    identifier(&with_index(name, index), case)
}

/// Steps 2 to 5 on a text that has no `%s`: the stem of a file name.
pub(super) fn identifier(text: &str, case: Case) -> Option<Converted> {
    let characters = characters(text);
    if characters.is_empty() {
        return None;
    }
    let mut changed = characters != text;
    let mut name = apply_case(&characters, case);
    if name.starts_with(|c: char| c.is_ascii_digit()) {
        name.insert(0, '_');
        changed = true;
    }
    if is_reserved(&name) {
        name.push('_');
    }
    Some(Converted { name, changed })
}

/// The name of a field's enum (ADR 0071): the struct's name, then Pascal case
/// of the field's name (steps 1 to 3, the field standing for the whole
/// element). The name starts with the struct's, so a leading digit of the
/// field's is no leading digit, and it is not a rename. `None` when nothing is
/// left of the field's name.
pub(super) fn suffix(struct_name: &str, field: &str) -> Option<Converted> {
    let text = with_index(field, Index::Whole);
    let characters = characters(&text);
    if characters.is_empty() {
        return None;
    }
    let mut name = format!("{struct_name}{}", apply_case(&characters, Case::Pascal));
    if is_reserved(&name) {
        name.push('_');
    }
    Some(Converted {
        name,
        changed: characters != text,
    })
}

/// The name as a module of a program: what `erebos svd` names its output
/// after (ADR 0070). `None` when nothing is left of it.
pub(super) fn module_name(stem: &str) -> Option<String> {
    identifier(stem, Case::Lower).map(|converted| converted.name)
}

/// Step 1: `[%s]` and `%s` replaced by the index, or removed.
fn with_index(name: &str, index: Index) -> String {
    let index = match index {
        Index::Whole => "",
        Index::Element(index) => index,
    };
    let mut out = String::with_capacity(name.len());
    let mut rest = name;
    while let Some(c) = rest.chars().next() {
        if let Some(after) = rest
            .strip_prefix("[%s]")
            .or_else(|| rest.strip_prefix("%s"))
        {
            out.push_str(index);
            rest = after;
        } else {
            out.push(c);
            rest = &rest[c.len_utf8()..];
        }
    }
    out
}

/// Step 2: a character that is no ASCII letter, digit or `_` becomes `_`,
/// each run of `_` becomes one, and `_` at either end goes.
fn characters(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        let c = if c.is_ascii_alphanumeric() { c } else { '_' };
        if c != '_' || !out.ends_with('_') {
            out.push(c);
        }
    }
    out.trim_matches('_').to_owned()
}

/// Step 3. `text` holds ASCII letters, digits and `_` alone.
fn apply_case(text: &str, case: Case) -> String {
    match case {
        Case::Keep => text.to_owned(),
        Case::Lower => text.to_ascii_lowercase(),
        Case::Pascal => text
            .split('_')
            .map(|part| {
                let mut chars = part.chars();
                chars
                    .next()
                    .map(|first| {
                        first.to_ascii_uppercase().to_string()
                            + &chars.as_str().to_ascii_lowercase()
                    })
                    .unwrap_or_default()
            })
            .collect(),
    }
}

/// Step 5: a keyword of the lexer, `core`, or the name of a built-in type.
/// Words recognized by position alone (`read`, `in`, ...) are names.
fn is_reserved(name: &str) -> bool {
    TokenKind::keyword(name).is_some()
        || name == "core"
        || matches!(
            name,
            "bool"
                | "u8"
                | "u16"
                | "u32"
                | "u64"
                | "i8"
                | "i16"
                | "i32"
                | "i64"
                | "usize"
                | "isize"
                | "Option"
                | "Result"
        )
}

/// The names taken in one scope: the items of a module, the entries of a
/// peripheral type. The later item to want a name gets `_` after it
/// (step 6).
#[derive(Default)]
pub(super) struct Scope {
    taken: HashMap<String, Span>,
    /// For a name that was asked for, how many `_` the next item asking for
    /// it starts from: every shorter one is taken, and a name is never
    /// freed, so many items of one name are not tried against each other.
    next: HashMap<String, usize>,
}

impl Scope {
    /// Takes `name` for the item of the SVD element at `span`: `name`
    /// itself if it is free, otherwise with as many `_` after it as it
    /// takes. When it is renamed, also gives the element that has `name`.
    pub fn claim(&mut self, name: String, span: Span) -> (String, Option<Span>) {
        let first = self.taken.get(&name).copied();
        let mut underscores = self.next.get(&name).copied().unwrap_or(0);
        let mut claimed = name.clone();
        claimed.push_str(&"_".repeat(underscores));
        while self.taken.contains_key(&claimed) {
            claimed.push('_');
            underscores += 1;
        }
        self.taken.insert(claimed.clone(), span);
        self.next.insert(name, underscores + 1);
        (claimed, first)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::span::SourceMap;

    fn name(svd: &str, index: Index, case: Case) -> String {
        convert(svd, index, case).unwrap().name
    }

    fn lower(svd: &str) -> String {
        name(svd, Index::Whole, Case::Lower)
    }

    #[test]
    fn the_index_replaces_or_is_removed() {
        // ADR 0071, step 1.
        assert_eq!(
            name("CH%s_CONF0", Index::Element("2"), Case::Lower),
            "ch2_conf0"
        );
        assert_eq!(
            name("CH%s_CONF0", Index::Element("A"), Case::Lower),
            "cha_conf0"
        );
        assert_eq!(name("FOO[%s]", Index::Element("2"), Case::Lower), "foo2");
        assert_eq!(lower("CH%s_CONF0"), "ch_conf0");
        assert_eq!(lower("PRIORITY[%s]"), "priority");
        assert_eq!(lower("%s_X"), "x");
        // An index is not searched for `%s` again: its `%` is a character like any.
        assert_eq!(name("A%s", Index::Element("%s"), Case::Keep), "A_s");
    }

    #[test]
    fn characters_that_are_no_identifier_become_one_underscore() {
        let converted = convert("A.B--C", Index::Whole, Case::Lower).unwrap();
        assert_eq!(converted.name, "a_b_c");
        assert!(converted.changed);
        assert_eq!(lower("_X_"), "x");
        assert!(convert("_X_", Index::Whole, Case::Lower).unwrap().changed);
        // A letter that is not ASCII is no character of the lexer.
        assert_eq!(lower("CAFÉ1"), "caf_1");
        // The index step is not a rename: the SVD wrote `%s`.
        assert!(
            !convert("CH%s", Index::Element("2"), Case::Lower)
                .unwrap()
                .changed
        );
        assert!(!convert("A", Index::Whole, Case::Keep).unwrap().changed);
    }

    #[test]
    fn nothing_left_is_no_name() {
        for text in ["", "...", "_", "-", "%s", "[%s]"] {
            assert_eq!(convert(text, Index::Whole, Case::Lower), None, "{text:?}");
        }
    }

    #[test]
    fn cases() {
        assert_eq!(name("RND_DATA", Index::Whole, Case::Pascal), "RndData");
        assert_eq!(name("UART0", Index::Whole, Case::Pascal), "Uart0");
        assert_eq!(name("rtc_cntl", Index::Whole, Case::Pascal), "RtcCntl");
        assert_eq!(name("Uart_0", Index::Whole, Case::Keep), "Uart_0");
        assert_eq!(lower("RxFifo_RST"), "rxfifo_rst");
    }

    #[test]
    fn a_leading_digit_gets_an_underscore() {
        let pascal = convert("1MHZ", Index::Whole, Case::Pascal).unwrap();
        assert_eq!((pascal.name.as_str(), pascal.changed), ("_1mhz", true));
        assert_eq!(lower("0"), "_0");
        assert_eq!(name("3DES", Index::Whole, Case::Keep), "_3DES");
    }

    #[test]
    fn names_with_a_meaning_get_an_underscore() {
        assert_eq!(lower("CONTINUE"), "continue_");
        assert_eq!(lower("MATCH"), "match_");
        // `critical` is a keyword with M4.3 (ADR 0078).
        assert_eq!(lower("CRITICAL"), "critical_");
        assert_eq!(lower("CORE"), "core_");
        assert_eq!(lower("U8"), "u8_");
        assert_eq!(name("OPTION", Index::Whole, Case::Pascal), "Option_");
        assert_eq!(name("match", Index::Whole, Case::Keep), "match_");
        // A keyword is not renamed in the sense of the warning.
        assert!(
            !convert("CONTINUE", Index::Whole, Case::Lower)
                .unwrap()
                .changed
        );
        // Words recognized by position alone are names.
        for word in ["IN", "READ", "WRITE", "STEP", "AT", "OVER", "CLEARS"] {
            assert_eq!(lower(word), word.to_ascii_lowercase());
        }
    }

    #[test]
    fn a_file_stem_makes_a_module_name() {
        // ADR 0070.
        assert_eq!(module_name("esp32-c3").as_deref(), Some("esp32_c3"));
        assert_eq!(module_name("32bit").as_deref(), Some("_32bit"));
        assert_eq!(module_name("match").as_deref(), Some("match_"));
        assert_eq!(module_name("---"), None);
    }

    #[test]
    fn a_scope_adds_underscores_to_the_later_item() {
        let mut sources = SourceMap::default();
        let file = sources.add("x.svd", "0123456789");
        let at = |lo| Span::new(file, lo, lo + 1);
        let mut scope = Scope::default();
        assert_eq!(scope.claim("a".to_owned(), at(0)), ("a".to_owned(), None));
        assert_eq!(
            scope.claim("a".to_owned(), at(1)),
            ("a_".to_owned(), Some(at(0)))
        );
        assert_eq!(
            scope.claim("a".to_owned(), at(2)),
            ("a__".to_owned(), Some(at(0)))
        );
        // The renamed item holds its new name.
        assert_eq!(
            scope.claim("a_".to_owned(), at(3)),
            ("a___".to_owned(), Some(at(1)))
        );
    }
}
