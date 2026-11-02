//! The format of the generated text (ADR 0070): numbers, and comments
//! wrapped at 79 columns.

/// A comment line is at most this many characters, its indentation and
/// `// ` included.
const WIDTH: usize = 79;

/// A number as the generator writes offsets, addresses and steps:
/// hexadecimal, `0x` then upper-case digits, in groups of four from the
/// right separated by `_` when longer than four (`0x6000_0000`, `0x20`).
pub(super) fn hex(value: u64) -> String {
    let digits = format!("{value:X}");
    let mut out = String::from("0x");
    for (i, digit) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(4) {
            out.push('_');
        }
        out.push(digit);
    }
    out
}

/// `text` as comment lines, each starting with `prefix` (`"// "` or
/// `"// svd: "`) after `indent` spaces: its white space collapsed and its
/// words wrapped so that no line passes 79 characters. A word longer than a
/// line stands alone on its own, uncut. Nothing for a text of white space.
pub(super) fn comment(indent: usize, prefix: &str, text: &str) -> Vec<String> {
    let room = WIDTH.saturating_sub(indent + prefix.chars().count());
    let mut lines: Vec<String> = Vec::new();
    let mut line = String::new();
    let mut length = 0;
    for word in text.split_whitespace() {
        let word_length = word.chars().count();
        if length > 0 && length + 1 + word_length > room {
            lines.push(std::mem::take(&mut line));
            length = 0;
        }
        if length > 0 {
            line.push(' ');
            length += 1;
        }
        line.push_str(word);
        length += word_length;
    }
    if length > 0 {
        lines.push(line);
    }
    let margin = " ".repeat(indent);
    lines
        .into_iter()
        .map(|line| format!("{margin}{prefix}{line}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_are_grouped_by_four_from_the_right() {
        assert_eq!(hex(0), "0x0");
        assert_eq!(hex(0x20), "0x20");
        assert_eq!(hex(0xFFFF), "0xFFFF");
        assert_eq!(hex(0x1_0000), "0x1_0000");
        assert_eq!(hex(0x6000_0000), "0x6000_0000");
        assert_eq!(hex(0x0C20_0000), "0xC20_0000");
        assert_eq!(hex(0x1_0000_0000), "0x1_0000_0000");
        assert_eq!(hex(u64::MAX), "0xFFFF_FFFF_FFFF_FFFF");
    }

    #[test]
    fn a_comment_collapses_white_space_and_wraps_at_79_columns() {
        assert_eq!(
            comment(4, "// ", "  one \n\t two   three "),
            ["    // one two three"]
        );
        assert!(comment(0, "// ", " \n ").is_empty());
        let words = "word ".repeat(40);
        let lines = comment(8, "// ", &words);
        assert!(lines.len() > 1);
        assert!(lines.iter().all(|line| line.chars().count() <= 79));
        // Full lines: one more word would not fit.
        assert_eq!(lines[0].chars().count(), 8 + 3 + 13 * 5 - 1);
    }

    #[test]
    fn a_word_longer_than_a_line_stands_alone() {
        let long = "x".repeat(100);
        assert_eq!(
            comment(0, "// ", &format!("before {long} after")),
            [
                "// before".to_owned(),
                format!("// {long}"),
                "// after".to_owned()
            ]
        );
    }

    #[test]
    fn each_line_of_a_note_starts_with_the_prefix() {
        let lines = comment(4, "// svd: ", &"note ".repeat(30));
        assert!(lines.len() > 1);
        assert!(
            lines
                .iter()
                .all(|line| line.starts_with("    // svd: note"))
        );
    }
}
