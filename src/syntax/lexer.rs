//! Hand-written lexer (ADR 0004). Produces tokens with spans; invalid input
//! is reported and skipped so lexing always reaches the end of the file.

use std::ops::Range;
use std::str::Chars;

use crate::diag::{Diagnostic, Diagnostics};
use crate::span::{FileId, Span};
use crate::syntax::token::{Token, TokenKind};

/// Lexes a whole file. The result always ends with a single `Eof` token.
pub fn lex(file: FileId, text: &str, diags: &mut Diagnostics) -> Vec<Token> {
    let mut lexer = Lexer {
        file,
        text,
        pos: 0,
        diags,
    };
    let mut tokens = Vec::new();
    while let Some(token) = lexer.next_token() {
        tokens.push(token);
    }
    tokens.push(Token {
        kind: TokenKind::Eof,
        span: Span::new(file, text.len(), text.len()),
    });
    tokens
}

struct Lexer<'a> {
    file: FileId,
    text: &'a str,
    pos: usize,
    diags: &'a mut Diagnostics,
}

impl Lexer<'_> {
    fn peek(&self) -> Option<char> {
        self.text[self.pos..].chars().next()
    }

    fn peek_second(&self) -> Option<char> {
        self.text[self.pos..].chars().nth(1)
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.pos += c.len_utf8();
        Some(c)
    }

    fn span_from(&self, start: usize) -> Span {
        Span::new(self.file, start, self.pos)
    }

    fn next_token(&mut self) -> Option<Token> {
        loop {
            self.skip_trivia();
            let start = self.pos;
            let c = self.bump()?;

            let kind = match c {
                '(' => TokenKind::LParen,
                ')' => TokenKind::RParen,
                '{' => TokenKind::LBrace,
                '}' => TokenKind::RBrace,
                '[' => TokenKind::LBracket,
                ']' => TokenKind::RBracket,
                ':' if self.eat(':') => TokenKind::ColonColon,
                ':' => TokenKind::Colon,
                ',' => TokenKind::Comma,
                ';' => TokenKind::Semi,
                '.' if self.eat('.') => self.with_eq(TokenKind::DotDot, TokenKind::DotDotEq),
                '.' => TokenKind::Dot,
                '?' => TokenKind::Question,
                '#' => TokenKind::Pound,
                '!' if self.eat('=') => TokenKind::Ne,
                '!' => TokenKind::Bang,
                '-' if self.eat('>') => TokenKind::Arrow,
                '+' => self.with_eq(TokenKind::Plus, TokenKind::PlusEq),
                '-' => self.with_eq(TokenKind::Minus, TokenKind::MinusEq),
                '*' => self.with_eq(TokenKind::Star, TokenKind::StarEq),
                // Comments were skipped above, so this `/` divides.
                '/' => self.with_eq(TokenKind::Slash, TokenKind::SlashEq),
                '%' => self.with_eq(TokenKind::Percent, TokenKind::PercentEq),
                '&' if self.eat('&') => TokenKind::AndAnd,
                '&' => self.with_eq(TokenKind::Amp, TokenKind::AmpEq),
                '|' if self.eat('|') => TokenKind::OrOr,
                '|' => self.with_eq(TokenKind::Pipe, TokenKind::PipeEq),
                '^' => self.with_eq(TokenKind::Caret, TokenKind::CaretEq),
                '=' if self.eat('>') => TokenKind::FatArrow,
                '=' => self.with_eq(TokenKind::Eq, TokenKind::EqEq),
                '<' if self.eat('<') => self.with_eq(TokenKind::Shl, TokenKind::ShlEq),
                '<' => self.with_eq(TokenKind::Lt, TokenKind::Le),
                '>' if self.eat('>') => self.with_eq(TokenKind::Shr, TokenKind::ShrEq),
                '>' => self.with_eq(TokenKind::Gt, TokenKind::Ge),
                '0'..='9' => {
                    self.number(start);
                    TokenKind::Int
                }
                'b' if self.peek() == Some('\'') => {
                    self.byte(start);
                    TokenKind::Byte
                }
                'b' if self.peek() == Some('"') => {
                    self.bump(); // opening quote
                    self.quoted(start, "byte string", "b\"");
                    TokenKind::ByteString
                }
                '"' => {
                    self.quoted(start, "string literal", "\"");
                    TokenKind::String
                }
                c if is_ident_start(c) => {
                    while self.peek().is_some_and(is_ident_continue) {
                        self.bump();
                    }
                    TokenKind::keyword(&self.text[start..self.pos]).unwrap_or(TokenKind::Ident)
                }
                c => {
                    self.diags.push(
                        Diagnostic::error(
                            "lexer::unexpected-character",
                            format!("unexpected character {c:?}"),
                        )
                        .label(self.span_from(start), "not valid here"),
                    );
                    continue;
                }
            };
            return Some(Token {
                kind,
                span: self.span_from(start),
            });
        }
    }

    fn eat(&mut self, c: char) -> bool {
        let matches = self.peek() == Some(c);
        if matches {
            self.bump();
        }
        matches
    }

    /// `op` or, if followed by `=`, the compound assignment `op_eq`.
    fn with_eq(&mut self, op: TokenKind, op_eq: TokenKind) -> TokenKind {
        if self.eat('=') { op_eq } else { op }
    }

    fn error(&mut self, code: &'static str, message: &str, span: Span, label: &str) {
        self.diags
            .push(Diagnostic::error(code, message).label(span, label));
    }

    /// Consumes the rest of an integer literal and validates it (ADR 0016).
    fn number(&mut self, start: usize) {
        while self
            .peek()
            .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            self.bump();
        }
        let span = self.span_from(start);
        match int_value(&self.text[start..self.pos]) {
            Ok(_) => {}
            Err(IntError::Empty) => self.error(
                "lexer::empty-int",
                "missing digits after the base prefix",
                span,
                "no digits",
            ),
            Err(IntError::InvalidDigit(base)) => self.error(
                "lexer::invalid-digit",
                &format!("invalid digit for a base {base} literal"),
                span,
                "invalid digit",
            ),
            Err(IntError::LeadingZero) => self.diags.push(
                Diagnostic::error(
                    "lexer::leading-zero",
                    "decimal literals cannot have leading zeros",
                )
                .label(span, "leading zero")
                .help("write octal as `0o17`; in C, `017` would silently be 15"),
            ),
            Err(IntError::TooLarge) => self.error(
                "lexer::int-too-large",
                "integer literal is too large",
                span,
                "does not fit in 64 bits",
            ),
        }
    }

    /// Consumes a byte literal `b'x'` (the `b` is already consumed).
    fn byte(&mut self, start: usize) {
        self.bump(); // opening quote
        loop {
            match self.peek() {
                Some('\'') => {
                    self.bump();
                    break;
                }
                Some('\\') => {
                    self.bump();
                    self.bump();
                }
                Some('\n') | None => {
                    self.error(
                        "lexer::unterminated-byte",
                        "unterminated byte literal",
                        self.span_from(start),
                        "missing closing `'`",
                    );
                    return;
                }
                Some(_) => {
                    self.bump();
                }
            }
        }
        let span = self.span_from(start);
        if let Err(e) = byte_value(&self.text[start..self.pos]) {
            let message = match e {
                ByteError::Unterminated => "unterminated byte literal",
                ByteError::Empty => "empty byte literal",
                ByteError::TooMany => "a byte literal holds exactly one byte",
                ByteError::NotAscii => "only printable ASCII can be written directly",
                ByteError::BadEscape => "unknown escape sequence",
            };
            self.diags.push(
                Diagnostic::error("lexer::invalid-byte", message)
                    .label(span, "invalid byte literal")
                    .help(r#"escapes: \n \r \t \\ \' \" \0 and \xNN for any byte (ADR 0016)"#),
            );
        }
    }

    /// Consumes the rest of a byte string `b"..."` (ADR 0031) or of a string
    /// literal `"..."` (ADR 0079), after its opening quote: both end on
    /// their line and hold the characters and escapes of a byte string.
    /// `what` names the token in the diagnostics, and `open` is the text
    /// that opened it, `b"` or `"`.
    fn quoted(&mut self, start: usize, what: &str, open: &str) {
        loop {
            match self.peek() {
                Some('"') => {
                    self.bump();
                    break;
                }
                Some('\\') => {
                    self.bump();
                    if !matches!(self.peek(), Some('\n') | None) {
                        self.bump();
                    }
                }
                Some('\n') | None => {
                    let help = if open.starts_with('b') {
                        r"a byte string ends on its line; write a line break as `\n` (ADR 0031)"
                    } else {
                        r"a string literal ends on its line; write a line break as `\n`, or give `asm` one string per line (ADR 0079)"
                    };
                    self.diags.push(
                        Diagnostic::error(
                            "lexer::unterminated-byte-string",
                            format!("unterminated {what}"),
                        )
                        .label(self.span_from(start), "missing closing `\"`")
                        .help(help),
                    );
                    return;
                }
                Some(_) => {
                    self.bump();
                }
            }
        }
        let Err(errors) = quoted_value(&self.text[start..self.pos], open) else {
            return;
        };
        // Each bad character or escape, so that one pass finds them all.
        for (e, at) in errors {
            let message = match e {
                ByteError::NotAscii => "only printable ASCII can be written directly",
                ByteError::BadEscape => "unknown escape sequence",
                ByteError::Unterminated | ByteError::Empty | ByteError::TooMany => {
                    unreachable!("a terminated {what} holds any number of bytes")
                }
            };
            self.diags.push(
                Diagnostic::error("lexer::invalid-byte", message)
                    .label(
                        Span::new(self.file, start + at.start, start + at.end),
                        format!("invalid in a {what}"),
                    )
                    .help(r#"escapes: \n \r \t \\ \' \" \0 and \xNN for any byte (ADR 0016)"#),
            );
        }
    }

    /// Skips whitespace and comments.
    fn skip_trivia(&mut self) {
        loop {
            match (self.peek(), self.peek_second()) {
                (Some(c), _) if c.is_whitespace() => {
                    self.bump();
                }
                (Some('/'), Some('/')) => {
                    while self.peek().is_some_and(|c| c != '\n') {
                        self.bump();
                    }
                }
                (Some('/'), Some('*')) => self.skip_block_comment(),
                _ => return,
            }
        }
    }

    /// Block comments nest, so code containing comments can be commented out.
    fn skip_block_comment(&mut self) {
        let start = self.pos;
        self.pos += 2;
        let mut depth = 1;
        while depth > 0 {
            match (self.bump(), self.peek()) {
                (Some('/'), Some('*')) => {
                    self.bump();
                    depth += 1;
                }
                (Some('*'), Some('/')) => {
                    self.bump();
                    depth -= 1;
                }
                (Some(_), _) => {}
                (None, _) => {
                    self.diags.push(
                        Diagnostic::error(
                            "lexer::unterminated-comment",
                            "unterminated block comment",
                        )
                        .label(
                            Span::new(self.file, start, start + 2),
                            "comment starts here",
                        )
                        .help("add `*/` to close the comment"),
                    );
                    return;
                }
            }
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum IntError {
    Empty,
    InvalidDigit(u32),
    LeadingZero,
    TooLarge,
}

/// The value of an integer literal's source text (ADR 0016).
pub fn int_value(text: &str) -> Result<u64, IntError> {
    let (base, digits) = match text.get(..2) {
        Some("0b") => (2, &text[2..]),
        Some("0o") => (8, &text[2..]),
        Some("0x") => (16, &text[2..]),
        _ => (10, text),
    };
    let digits = digits.replace('_', "");
    if digits.is_empty() {
        return Err(IntError::Empty);
    }
    if !digits.chars().all(|c| c.is_digit(base)) {
        return Err(IntError::InvalidDigit(base));
    }
    if base == 10 && digits.len() > 1 && digits.starts_with('0') {
        return Err(IntError::LeadingZero);
    }
    u64::from_str_radix(&digits, base).map_err(|_| IntError::TooLarge)
}

#[derive(Debug, PartialEq, Eq)]
pub enum ByteError {
    /// No closing `'`; the lexer reports it before the value is asked for.
    Unterminated,
    Empty,
    TooMany,
    NotAscii,
    BadEscape,
}

/// The value of a byte literal's source text, including `b'` and `'`.
pub fn byte_value(text: &str) -> Result<u8, ByteError> {
    let Some(inner) = text
        .strip_prefix("b'")
        .and_then(|rest| rest.strip_suffix('\''))
    else {
        return Err(ByteError::Unterminated);
    };
    let mut chars = inner.chars();
    let value = match chars.next() {
        None => return Err(ByteError::Empty),
        Some(c) => one_byte(c, &mut chars)?,
    };
    if chars.next().is_some() {
        return Err(ByteError::TooMany);
    }
    Ok(value)
}

/// The bytes of a byte string's source text, including `b"` and `"`
/// (ADR 0031); or every error, each with the range of the text at fault.
pub fn byte_string_value(text: &str) -> Result<Vec<u8>, Vec<(ByteError, Range<usize>)>> {
    quoted_value(text, "b\"")
}

/// The bytes of a string literal's source text, including both quotes
/// (ADR 0079), by the rules of a byte string.
pub fn string_value(text: &str) -> Result<Vec<u8>, Vec<(ByteError, Range<usize>)>> {
    quoted_value(text, "\"")
}

/// The bytes of a string literal, and where each starts (`string_bytes`).
pub type StringBytes = (Vec<u8>, Vec<usize>);

/// The bytes of a string literal's source text, as `string_value`, and where
/// each starts in the text: its offset from the opening quote, which an
/// escape's backslash gives. One more offset follows, the closing quote's, so
/// that `starts[i]..starts[j]` is the text of the bytes `i..j` (ADR 0079).
pub fn string_bytes(text: &str) -> Result<StringBytes, Vec<(ByteError, Range<usize>)>> {
    quoted_bytes(text, "\"")
}

/// The bytes of the source text of a byte string or a string literal, which
/// starts with `open`, `b"` or `"`, and ends with `"`.
fn quoted_value(text: &str, open: &str) -> Result<Vec<u8>, Vec<(ByteError, Range<usize>)>> {
    quoted_bytes(text, open).map(|(bytes, _)| bytes)
}

/// `quoted_value`, with the offset in `text` of each byte, and of the
/// closing quote.
fn quoted_bytes(text: &str, open: &str) -> Result<StringBytes, Vec<(ByteError, Range<usize>)>> {
    let Some(inner) = text
        .strip_prefix(open)
        .and_then(|rest| rest.strip_suffix('"'))
    else {
        return Err(vec![(ByteError::Unterminated, 0..text.len())]);
    };
    let mut bytes = Vec::new();
    let mut starts = Vec::new();
    let mut errors = Vec::new();
    let mut chars = inner.chars();
    // Offsets in `text`, past the opening quote.
    let offset = |rest: &str| open.len() + inner.len() - rest.len();
    while let Some(c) = chars.next() {
        let start = offset(chars.as_str()) - c.len_utf8();
        match one_byte(c, &mut chars) {
            Ok(byte) => {
                bytes.push(byte);
                starts.push(start);
            }
            Err(e) => errors.push((e, start..offset(chars.as_str()))),
        }
    }
    starts.push(open.len() + inner.len());
    if errors.is_empty() {
        Ok((bytes, starts))
    } else {
        Err(errors)
    }
}

/// One byte of a literal (ADR 0016), starting with `c`, followed by
/// `rest`: printable ASCII, or an escape.
fn one_byte(c: char, rest: &mut Chars) -> Result<u8, ByteError> {
    Ok(match c {
        '\\' => match rest.next() {
            Some('n') => b'\n',
            Some('r') => b'\r',
            Some('t') => b'\t',
            Some('0') => 0,
            Some('\\') => b'\\',
            Some('\'') => b'\'',
            Some('"') => b'"',
            Some('x') => {
                let hex: String = rest.by_ref().take(2).collect();
                // Checked here because `from_str_radix` accepts a sign (`+F`).
                if hex.len() != 2 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
                    return Err(ByteError::BadEscape);
                }
                u8::from_str_radix(&hex, 16).map_err(|_| ByteError::BadEscape)?
            }
            _ => return Err(ByteError::BadEscape),
        },
        c if c == ' ' || c.is_ascii_graphic() => c as u8,
        _ => return Err(ByteError::NotAscii),
    })
}

fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

fn is_ident_continue(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::span::SourceMap;

    fn kinds(text: &str) -> (Vec<TokenKind>, Diagnostics) {
        let mut sources = SourceMap::default();
        let file = sources.add("test.ers", text);
        let mut diags = Diagnostics::default();
        let tokens = lex(file, text, &mut diags);
        (tokens.into_iter().map(|t| t.kind).collect(), diags)
    }

    #[test]
    fn keywords_identifiers_and_punctuation() {
        use TokenKind::*;
        let (kinds, diags) = kinds("entry fn main_1() -> ! { loop {} } panic");
        assert!(!diags.has_errors());
        assert_eq!(
            kinds,
            [
                Entry, Fn, Ident, LParen, RParen, Arrow, Bang, LBrace, Loop, LBrace, RBrace,
                RBrace, Panic, Eof
            ]
        );
    }

    #[test]
    fn module_keywords() {
        use TokenKind::*;
        // `core` is a reserved name, not a keyword (ADR 0049, 0052).
        let (kinds, diags) = kinds("pub mod m {} crate::super::x use core");
        assert!(!diags.has_errors());
        assert_eq!(
            kinds,
            [
                Pub, Mod, Ident, LBrace, RBrace, Crate, ColonColon, Super, ColonColon, Ident, Use,
                Ident, Eof
            ]
        );
    }

    #[test]
    fn paths_and_enums() {
        use TokenKind::*;
        let (kinds, diags) = kinds("enum E: u8 { A = 0 } E::A :::");
        assert!(!diags.has_errors());
        assert_eq!(
            kinds,
            [
                Enum, Ident, Colon, Ident, LBrace, Ident, Eq, Int, RBrace, Ident, ColonColon,
                Ident, ColonColon, Colon, Eof
            ]
        );
    }

    #[test]
    fn match_arms_and_wildcards() {
        use TokenKind::*;
        let (kinds, diags) = kinds("match x { _ => 0, _x => 1 } a == b = c");
        assert!(!diags.has_errors());
        assert_eq!(
            kinds,
            [
                Match, Ident, LBrace, Underscore, FatArrow, Int, Comma, Ident, FatArrow, Int,
                RBrace, Ident, EqEq, Ident, Eq, Ident, Eof
            ]
        );
    }

    #[test]
    fn question_marks() {
        use TokenKind::*;
        let (kinds, diags) = kinds("f()?? x?.y");
        assert!(!diags.has_errors());
        assert_eq!(
            kinds,
            [
                Ident, LParen, RParen, Question, Question, Ident, Question, Dot, Ident, Eof
            ]
        );
    }

    #[test]
    fn ranges_and_rests() {
        use TokenKind::*;
        // An integer literal never holds a `.`, so `0..=1` is three tokens
        // (ADR 0059).
        let (kinds, diags) = kinds("0..=1 7..=x .. ... a.b .V");
        assert!(!diags.has_errors());
        assert_eq!(
            kinds,
            [
                Int, DotDotEq, Int, Int, DotDotEq, Ident, DotDot, DotDot, Dot, Ident, Dot, Ident,
                Dot, Ident, Eof
            ]
        );
    }

    #[test]
    fn comments_are_skipped_and_nest() {
        let (kinds, diags) = kinds("// line\n/* a /* nested */ b */ fn");
        assert!(!diags.has_errors());
        assert_eq!(kinds, [TokenKind::Fn, TokenKind::Eof]);
    }

    #[test]
    fn unterminated_comment_is_reported() {
        let (kinds, diags) = kinds("fn /* /* */");
        assert_eq!(kinds, [TokenKind::Fn, TokenKind::Eof]);
        assert_eq!(diags.into_vec()[0].code, "lexer::unterminated-comment");
    }

    #[test]
    fn unexpected_characters_are_reported_and_skipped() {
        let (kinds, diags) = kinds("fn ç @ main");
        assert_eq!(kinds, [TokenKind::Fn, TokenKind::Ident, TokenKind::Eof]);
        let codes: Vec<_> = diags.into_vec().iter().map(|d| d.code).collect();
        assert_eq!(
            codes,
            ["lexer::unexpected-character", "lexer::unexpected-character"]
        );
    }

    #[test]
    fn integer_values() {
        assert_eq!(int_value("42"), Ok(42));
        assert_eq!(int_value("0"), Ok(0));
        assert_eq!(int_value("1_000"), Ok(1000));
        assert_eq!(int_value("0x2A"), Ok(42));
        assert_eq!(int_value("0o52"), Ok(42));
        assert_eq!(int_value("0b10_1010"), Ok(42));
        assert_eq!(int_value("0xFFFF_FFFF_FFFF_FFFF"), Ok(u64::MAX));
        assert_eq!(
            int_value("0x1_0000_0000_0000_0000"),
            Err(IntError::TooLarge)
        );
        assert_eq!(int_value("017"), Err(IntError::LeadingZero));
        assert_eq!(int_value("0b102"), Err(IntError::InvalidDigit(2)));
        assert_eq!(int_value("12ab"), Err(IntError::InvalidDigit(10)));
        assert_eq!(int_value("0x"), Err(IntError::Empty));
    }

    #[test]
    fn byte_values() {
        assert_eq!(byte_value("b'h'"), Ok(b'h'));
        assert_eq!(byte_value("b' '"), Ok(b' '));
        assert_eq!(byte_value(r"b'\n'"), Ok(b'\n'));
        assert_eq!(byte_value(r"b'\''"), Ok(b'\''));
        assert_eq!(byte_value(r"b'\x7F'"), Ok(0x7F));
        assert_eq!(byte_value("b''"), Err(ByteError::Empty));
        assert_eq!(byte_value("b'ab'"), Err(ByteError::TooMany));
        assert_eq!(byte_value("b'é'"), Err(ByteError::NotAscii));
        assert_eq!(byte_value(r"b'\q'"), Err(ByteError::BadEscape));
        assert_eq!(byte_value(r"b'\x4'"), Err(ByteError::BadEscape));
        // Tokens of unterminated literals, as the parser sees them.
        assert_eq!(byte_value("b'"), Err(ByteError::Unterminated));
        assert_eq!(byte_value("b'a"), Err(ByteError::Unterminated));
    }

    #[test]
    fn string_literals() {
        use TokenKind::*;
        let (kinds, diags) = kinds(r#"asm("nop", "a\"b") as asm_ b"x" "" b"" "#);
        assert!(!diags.has_errors());
        assert_eq!(
            kinds,
            [
                Asm, LParen, String, Comma, String, RParen, As, Ident, ByteString, String,
                ByteString, Eof
            ]
        );
    }

    #[test]
    fn string_values() {
        assert_eq!(string_value(r#""""#), Ok(vec![]));
        assert_eq!(
            string_value(r#""csrr {}, 0x\x41""#),
            Ok(b"csrr {}, 0xA".to_vec())
        );
        assert_eq!(
            string_value(r#""a\n\t\0\"\\""#),
            Ok(b"a\n\t\0\"\\".to_vec())
        );
        // The offsets are those of the text, past the opening quote.
        assert_eq!(
            string_value(r#""ab\qc""#),
            Err(vec![(ByteError::BadEscape, 3..5)])
        );
        assert_eq!(
            string_value("\"aé\""),
            Err(vec![(ByteError::NotAscii, 2..4)])
        );
        assert_eq!(
            string_value(r#""ab"#),
            Err(vec![(ByteError::Unterminated, 0..3)])
        );
    }

    /// Each byte's offset in the text, which an escape's backslash gives, then
    /// the closing quote's (ADR 0079).
    #[test]
    fn string_bytes_know_where_each_byte_starts() {
        assert_eq!(string_bytes(r#""""#), Ok((vec![], vec![1])));
        // `a`, `\x41`, `\n`, `\\`, `b` and the quote.
        assert_eq!(
            string_bytes(r#""a\x41\n\\b""#),
            Ok((b"aA\n\\b".to_vec(), vec![1, 2, 6, 8, 10, 11]))
        );
        assert_eq!(
            string_bytes(r#""ab\qc""#),
            Err(vec![(ByteError::BadEscape, 3..5)])
        );
    }

    #[test]
    fn string_literal_errors_are_those_of_byte_strings() {
        let (kinds, diags) = kinds("\"a\\qb\" \"open\nfn");
        assert_eq!(
            kinds,
            [
                TokenKind::String,
                TokenKind::String,
                TokenKind::Fn,
                TokenKind::Eof
            ]
        );
        let codes: Vec<_> = diags.into_vec().iter().map(|d| d.code).collect();
        assert_eq!(
            codes,
            ["lexer::invalid-byte", "lexer::unterminated-byte-string"]
        );
    }

    #[test]
    fn byte_string_values() {
        assert_eq!(byte_string_value(r#"b"""#), Ok(vec![]));
        assert_eq!(byte_string_value(r#"b"a\x00b""#), Ok(vec![b'a', 0, b'b']));
        assert_eq!(
            byte_string_value(r#"b"\n\r\t\\\'\"\0 '""#),
            Ok(b"\n\r\t\\'\"\0 '".to_vec())
        );
        assert_eq!(
            byte_string_value(r#"b"\xff\xC3\xA9""#),
            Ok(vec![0xFF, 0xC3, 0xA9])
        );
        assert_eq!(
            byte_string_value(r#"b"ab\qc""#),
            Err(vec![(ByteError::BadEscape, 4..6)])
        );
        assert_eq!(
            byte_string_value("b\"aé\""),
            Err(vec![(ByteError::NotAscii, 3..5)])
        );
        assert_eq!(
            byte_string_value(r#"b"\x4g""#),
            Err(vec![(ByteError::BadEscape, 2..6)])
        );
        // Every error, each at its own text.
        assert_eq!(
            byte_string_value("b\"\\qa\tb\\x+F\""),
            Err(vec![
                (ByteError::BadEscape, 2..4),
                (ByteError::NotAscii, 5..6),
                (ByteError::BadEscape, 7..11),
            ])
        );
        // The token of an unterminated byte string, as the parser sees it.
        assert_eq!(
            byte_string_value(r#"b"ab"#),
            Err(vec![(ByteError::Unterminated, 0..4)])
        );
    }
}
