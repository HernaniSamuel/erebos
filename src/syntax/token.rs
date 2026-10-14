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
    AlignOf,
    As,
    Break,
    Const,
    Continue,
    Else,
    Entry,
    False,
    Fn,
    If,
    Let,
    Loop,
    Mut,
    OffsetOf,
    Panic,
    Return,
    SizeOf,
    Struct,
    True,
    Volatile,
    While,

    // Punctuation
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Arrow,
    Colon,
    Comma,
    Semi,
    Dot,

    // Operators
    Bang,
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Amp,
    Pipe,
    Caret,
    Shl,
    Shr,
    Eq,
    PlusEq,
    MinusEq,
    StarEq,
    SlashEq,
    PercentEq,
    AmpEq,
    PipeEq,
    CaretEq,
    ShlEq,
    ShrEq,
    EqEq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    AndAnd,
    OrOr,

    Eof,
}

impl TokenKind {
    pub fn keyword(text: &str) -> Option<TokenKind> {
        Some(match text {
            "align_of" => TokenKind::AlignOf,
            "as" => TokenKind::As,
            "break" => TokenKind::Break,
            "const" => TokenKind::Const,
            "continue" => TokenKind::Continue,
            "else" => TokenKind::Else,
            "entry" => TokenKind::Entry,
            "false" => TokenKind::False,
            "fn" => TokenKind::Fn,
            "if" => TokenKind::If,
            "let" => TokenKind::Let,
            "loop" => TokenKind::Loop,
            "mut" => TokenKind::Mut,
            "offset_of" => TokenKind::OffsetOf,
            "panic" => TokenKind::Panic,
            "return" => TokenKind::Return,
            "size_of" => TokenKind::SizeOf,
            "struct" => TokenKind::Struct,
            "true" => TokenKind::True,
            "volatile" => TokenKind::Volatile,
            "while" => TokenKind::While,
            _ => return None,
        })
    }

    /// How the token is named in diagnostics.
    pub fn describe(self) -> &'static str {
        match self {
            TokenKind::Ident => "identifier",
            TokenKind::Int => "integer literal",
            TokenKind::Byte => "byte literal",
            TokenKind::AlignOf => "`align_of`",
            TokenKind::As => "`as`",
            TokenKind::Break => "`break`",
            TokenKind::Const => "`const`",
            TokenKind::Continue => "`continue`",
            TokenKind::Else => "`else`",
            TokenKind::Entry => "`entry`",
            TokenKind::False => "`false`",
            TokenKind::Fn => "`fn`",
            TokenKind::If => "`if`",
            TokenKind::Let => "`let`",
            TokenKind::Loop => "`loop`",
            TokenKind::Mut => "`mut`",
            TokenKind::OffsetOf => "`offset_of`",
            TokenKind::Panic => "`panic`",
            TokenKind::Return => "`return`",
            TokenKind::SizeOf => "`size_of`",
            TokenKind::Struct => "`struct`",
            TokenKind::True => "`true`",
            TokenKind::Volatile => "`volatile`",
            TokenKind::While => "`while`",
            TokenKind::LParen => "`(`",
            TokenKind::RParen => "`)`",
            TokenKind::LBrace => "`{`",
            TokenKind::RBrace => "`}`",
            TokenKind::LBracket => "`[`",
            TokenKind::RBracket => "`]`",
            TokenKind::Arrow => "`->`",
            TokenKind::Colon => "`:`",
            TokenKind::Comma => "`,`",
            TokenKind::Semi => "`;`",
            TokenKind::Dot => "`.`",
            TokenKind::Bang => "`!`",
            TokenKind::Plus => "`+`",
            TokenKind::Minus => "`-`",
            TokenKind::Star => "`*`",
            TokenKind::Slash => "`/`",
            TokenKind::Percent => "`%`",
            TokenKind::Amp => "`&`",
            TokenKind::Pipe => "`|`",
            TokenKind::Caret => "`^`",
            TokenKind::Shl => "`<<`",
            TokenKind::Shr => "`>>`",
            TokenKind::Eq => "`=`",
            TokenKind::PlusEq => "`+=`",
            TokenKind::MinusEq => "`-=`",
            TokenKind::StarEq => "`*=`",
            TokenKind::SlashEq => "`/=`",
            TokenKind::PercentEq => "`%=`",
            TokenKind::AmpEq => "`&=`",
            TokenKind::PipeEq => "`|=`",
            TokenKind::CaretEq => "`^=`",
            TokenKind::ShlEq => "`<<=`",
            TokenKind::ShrEq => "`>>=`",
            TokenKind::EqEq => "`==`",
            TokenKind::Ne => "`!=`",
            TokenKind::Lt => "`<`",
            TokenKind::Le => "`<=`",
            TokenKind::Gt => "`>`",
            TokenKind::Ge => "`>=`",
            TokenKind::AndAnd => "`&&`",
            TokenKind::OrOr => "`||`",
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
