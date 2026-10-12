//! Hand-written lexer (ADR 0004). Produces tokens with spans; invalid input
//! is reported and skipped so lexing always reaches the end of the file.

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
                '!' => TokenKind::Bang,
                '-' if self.peek() == Some('>') => {
                    self.bump();
                    TokenKind::Arrow
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
        let (kinds, diags) = kinds("fn ç - main");
        assert_eq!(kinds, [TokenKind::Fn, TokenKind::Ident, TokenKind::Eof]);
        let codes: Vec<_> = diags.into_vec().iter().map(|d| d.code).collect();
        assert_eq!(
            codes,
            ["lexer::unexpected-character", "lexer::unexpected-character"]
        );
    }
}
