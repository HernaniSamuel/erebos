//! The numbers and index lists SVD writes as text.

use super::EnumNumber;

/// Why a text is no number.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum NumberError {
    /// Not decimal, `0x` then hexadecimal, or `#` then binary.
    Syntax,
    /// Does not fit in 64 bits.
    TooLarge,
}

/// A number as SVD writes it: decimal, `0x` or `0X` then hexadecimal
/// digits, or `#` then binary digits, with white space around it allowed.
pub(super) fn parse_number(text: &str) -> Result<u64, NumberError> {
    let text = text.trim();
    let (digits, radix) =
        if let Some(rest) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
            (rest, 16)
        } else if let Some(rest) = text.strip_prefix('#') {
            (rest, 2)
        } else {
            (text, 10)
        };
    if digits.is_empty() || !digits.chars().all(|c| c.is_digit(radix)) {
        return Err(NumberError::Syntax);
    }
    u64::from_str_radix(digits, radix).map_err(|_| NumberError::TooLarge)
}

/// The value of an `enumeratedValue`: a number, or `#` then binary digits
/// of which some are `x` (don't care).
pub(super) fn parse_enum_number(text: &str) -> Result<EnumNumber, NumberError> {
    let text = text.trim();
    if let Some(bits) = text.strip_prefix('#')
        && !bits.is_empty()
        && bits.chars().all(|c| matches!(c, '0' | '1' | 'x' | 'X'))
        && bits.chars().any(|c| matches!(c, 'x' | 'X'))
    {
        return Ok(EnumNumber::DontCare(text.to_owned()));
    }
    parse_number(text).map(EnumNumber::Value)
}

/// A bit range of the form `[msb:lsb]`, as `(msb, lsb)`.
pub(super) fn parse_bit_range(text: &str) -> Option<(u32, u32)> {
    let inner = text.trim().strip_prefix('[')?.strip_suffix(']')?;
    let (msb, lsb) = inner.split_once(':')?;
    let number = |text: &str| u32::try_from(parse_number(text).ok()?).ok();
    Some((number(msb)?, number(lsb)?))
}

/// Why a `dimIndex` is not usable.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum IndexError {
    /// Neither a list, nor a range of numbers or of letters.
    Syntax,
    /// A range that goes down.
    Descending,
    /// A list or range of another length than `dim`: the length it has.
    Length(u64),
}

/// The indices `dimIndex` writes, `count` of them: a range of numbers
/// (`2-3`) or of capital or small letters (`A-C`), or a list (`A,B,C`,
/// `0,1,2`; one name is a list of one). The length is checked before any
/// index is made.
pub(super) fn parse_dim_index(text: &str, count: u32) -> Result<Vec<String>, IndexError> {
    let text = text.trim();
    let word = |s: &str| !s.is_empty() && s.chars().all(|c| c == '_' || c.is_ascii_alphanumeric());
    let length = |n: u64| {
        if n == u64::from(count) {
            Ok(())
        } else {
            Err(IndexError::Length(n))
        }
    };
    if text.contains(',') {
        let items: Vec<&str> = text.split(',').map(str::trim).collect();
        if !items.iter().all(|item| word(item)) {
            return Err(IndexError::Syntax);
        }
        length(items.len() as u64)?;
        return Ok(items.into_iter().map(str::to_owned).collect());
    }
    if let Some((lo, hi)) = text.split_once('-') {
        let (lo, hi) = (lo.trim(), hi.trim());
        let digits = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit());
        if digits(lo) && digits(hi) {
            let lo: u64 = lo.parse().map_err(|_| IndexError::Syntax)?;
            let hi: u64 = hi.parse().map_err(|_| IndexError::Syntax)?;
            if hi < lo {
                return Err(IndexError::Descending);
            }
            // `hi - lo + 1` overflows for `0-18446744073709551615`.
            length((hi - lo).saturating_add(1))?;
            return Ok((lo..=hi).map(|n| n.to_string()).collect());
        }
        let letter = |s: &str| {
            let mut chars = s.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) if c.is_ascii_alphabetic() => Some(c),
                _ => None,
            }
        };
        if let (Some(lo), Some(hi)) = (letter(lo), letter(hi))
            && lo.is_ascii_uppercase() == hi.is_ascii_uppercase()
        {
            if hi < lo {
                return Err(IndexError::Descending);
            }
            length(u64::from(hi as u8 - lo as u8) + 1)?;
            return Ok((lo..=hi).map(|c| c.to_string()).collect());
        }
        return Err(IndexError::Syntax);
    }
    if word(text) {
        length(1)?;
        return Ok(vec![text.to_owned()]);
    }
    Err(IndexError::Syntax)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_in_each_syntax() {
        assert_eq!(parse_number("42"), Ok(42));
        assert_eq!(parse_number("007"), Ok(7));
        assert_eq!(parse_number("0x2A"), Ok(42));
        assert_eq!(parse_number("0X2a"), Ok(42));
        assert_eq!(parse_number("#101010"), Ok(42));
        assert_eq!(parse_number("  0x10\n"), Ok(16));
        assert_eq!(parse_number("0xFFFFFFFFFFFFFFFF"), Ok(u64::MAX));
    }

    #[test]
    fn invalid_numbers() {
        for text in [
            "", " ", "0x", "#", "#2", "0b101", "1_000", "-1", "+1", "0x1g", "1.5", "12ab", "0xx1",
            "#1x0",
        ] {
            assert_eq!(parse_number(text), Err(NumberError::Syntax), "{text:?}");
        }
        assert_eq!(
            parse_number("0x1_0000_0000_0000_0000"),
            Err(NumberError::Syntax)
        );
        assert_eq!(
            parse_number("0x10000000000000000"),
            Err(NumberError::TooLarge)
        );
        assert_eq!(
            parse_number("18446744073709551616"),
            Err(NumberError::TooLarge)
        );
    }

    #[test]
    fn enumerated_values_with_dont_care_bits() {
        assert_eq!(parse_enum_number("5"), Ok(EnumNumber::Value(5)));
        assert_eq!(parse_enum_number("#101"), Ok(EnumNumber::Value(5)));
        assert_eq!(
            parse_enum_number(" #1x0 "),
            Ok(EnumNumber::DontCare("#1x0".to_owned()))
        );
        assert_eq!(
            parse_enum_number("#XX"),
            Ok(EnumNumber::DontCare("#XX".to_owned()))
        );
        assert_eq!(parse_enum_number("0x1x"), Err(NumberError::Syntax));
        assert_eq!(parse_enum_number("#"), Err(NumberError::Syntax));
    }

    #[test]
    fn bit_ranges() {
        assert_eq!(parse_bit_range("[7:4]"), Some((7, 4)));
        assert_eq!(parse_bit_range(" [0x1F : 0] "), Some((31, 0)));
        for text in [
            "7:4",
            "[7-4]",
            "[7:]",
            "[:4]",
            "[a:b]",
            "[]",
            "[4294967296:0]",
        ] {
            assert_eq!(parse_bit_range(text), None, "{text}");
        }
    }

    fn indices(text: &str, count: u32) -> Result<Vec<String>, IndexError> {
        parse_dim_index(text, count)
    }

    #[test]
    fn dim_index_ranges_and_lists() {
        assert_eq!(indices("2-3", 2), Ok(vec!["2".into(), "3".into()]));
        assert_eq!(
            indices("A-C", 3),
            Ok(vec!["A".into(), "B".into(), "C".into()])
        );
        assert_eq!(indices("a-b", 2), Ok(vec!["a".into(), "b".into()]));
        assert_eq!(
            indices("A, B,C", 3),
            Ok(vec!["A".into(), "B".into(), "C".into()])
        );
        assert_eq!(
            indices("0,1,2", 3),
            Ok(vec!["0".into(), "1".into(), "2".into()])
        );
        assert_eq!(indices("TX", 1), Ok(vec!["TX".into()]));
        assert_eq!(indices("7-7", 1), Ok(vec!["7".into()]));
    }

    #[test]
    fn invalid_dim_indices() {
        assert_eq!(indices("3-2", 2), Err(IndexError::Descending));
        assert_eq!(indices("C-A", 3), Err(IndexError::Descending));
        assert_eq!(indices("A-c", 3), Err(IndexError::Syntax));
        assert_eq!(indices("A-CC", 3), Err(IndexError::Syntax));
        assert_eq!(indices("1-A", 3), Err(IndexError::Syntax));
        assert_eq!(indices("A,,B", 3), Err(IndexError::Syntax));
        assert_eq!(indices("A B", 2), Err(IndexError::Syntax));
        assert_eq!(indices("", 1), Err(IndexError::Syntax));
        assert_eq!(indices("0-3", 3), Err(IndexError::Length(4)));
        assert_eq!(indices("A,B", 3), Err(IndexError::Length(2)));
        assert_eq!(indices("A", 2), Err(IndexError::Length(1)));
        // A range far larger than `dim` is refused before it is expanded.
        assert_eq!(
            indices("0-4000000000", 2),
            Err(IndexError::Length(4_000_000_001))
        );
        // The widest range a number allows does not overflow the length.
        assert_eq!(
            indices("0-18446744073709551615", 2),
            Err(IndexError::Length(u64::MAX))
        );
    }
}
