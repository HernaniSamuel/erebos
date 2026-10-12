use std::fmt;

use crate::span::Span;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenKind {
    Ident,
    /// Integer literal; its value is read from the source text.
    Int,
    /// Byte literal `b'x'`.
    Byte,

    // Keywords
    As,
    Entry,
    Fn,
    Let,
    Loop,
    Mut,
    Panic,

    // Punctuation
    LParen,
    RParen,
    LBrace,
    RBrace,
    Arrow,
    Colon,
    Semi,

    // Operators
    Bang,
    Plus,
    Minus,
    Star,
    Amp,
    Pipe,
    Caret,
    Shl,
    Shr,
    Eq,
    PlusEq,
    MinusEq,
    StarEq,
    AmpEq,
    PipeEq,
    CaretEq,
    ShlEq,
    ShrEq,

    Eof,
}

impl TokenKind {
    pub fn keyword(text: &str) -> Option<TokenKind> {
        Some(match text {
            "as" => TokenKind::As,
            "entry" => TokenKind::Entry,
            "fn" => TokenKind::Fn,
            "let" => TokenKind::Let,
            "loop" => TokenKind::Loop,
            "mut" => TokenKind::Mut,
            "panic" => TokenKind::Panic,
            _ => return None,
        })
    }

    /// How the token is named in diagnostics.
    pub fn describe(self) -> &'static str {
        match self {
            TokenKind::Ident => "identifier",
            TokenKind::Int => "integer literal",
            TokenKind::Byte => "byte literal",
            TokenKind::As => "`as`",
            TokenKind::Entry => "`entry`",
            TokenKind::Fn => "`fn`",
            TokenKind::Let => "`let`",
            TokenKind::Loop => "`loop`",
            TokenKind::Mut => "`mut`",
            TokenKind::Panic => "`panic`",
            TokenKind::LParen => "`(`",
            TokenKind::RParen => "`)`",
            TokenKind::LBrace => "`{`",
            TokenKind::RBrace => "`}`",
            TokenKind::Arrow => "`->`",
            TokenKind::Colon => "`:`",
            TokenKind::Semi => "`;`",
            TokenKind::Bang => "`!`",
            TokenKind::Plus => "`+`",
            TokenKind::Minus => "`-`",
            TokenKind::Star => "`*`",
            TokenKind::Amp => "`&`",
            TokenKind::Pipe => "`|`",
            TokenKind::Caret => "`^`",
            TokenKind::Shl => "`<<`",
            TokenKind::Shr => "`>>`",
            TokenKind::Eq => "`=`",
            TokenKind::PlusEq => "`+=`",
            TokenKind::MinusEq => "`-=`",
            TokenKind::StarEq => "`*=`",
            TokenKind::AmpEq => "`&=`",
            TokenKind::PipeEq => "`|=`",
            TokenKind::CaretEq => "`^=`",
            TokenKind::ShlEq => "`<<=`",
            TokenKind::ShrEq => "`>>=`",
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
