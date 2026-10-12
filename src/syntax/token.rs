use std::fmt;

use crate::span::Span;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenKind {
    Ident,

    // Keywords
    Entry,
    Fn,
    Loop,

    // Punctuation
    LParen,
    RParen,
    LBrace,
    RBrace,
    Arrow,
    Bang,

    Eof,
}

impl TokenKind {
    pub fn keyword(text: &str) -> Option<TokenKind> {
        Some(match text {
            "entry" => TokenKind::Entry,
            "fn" => TokenKind::Fn,
            "loop" => TokenKind::Loop,
            _ => return None,
        })
    }

    /// How the token is named in diagnostics.
    pub fn describe(self) -> &'static str {
        match self {
            TokenKind::Ident => "identifier",
            TokenKind::Entry => "`entry`",
            TokenKind::Fn => "`fn`",
            TokenKind::Loop => "`loop`",
            TokenKind::LParen => "`(`",
            TokenKind::RParen => "`)`",
            TokenKind::LBrace => "`{`",
            TokenKind::RBrace => "`}`",
            TokenKind::Arrow => "`->`",
            TokenKind::Bang => "`!`",
            TokenKind::Eof => "end of file",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?} {:?}", self.kind, self.span)
    }
}
