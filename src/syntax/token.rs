use std::fmt;

use crate::span::Span;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenKind {
    Ident,
    /// Integer literal; its value is read from the source text.
    Int,
    /// Byte literal `b'x'`.
    Byte,
    /// Byte string `b"..."` (ADR 0031).
    ByteString,

    // Keywords
    AlignOf,
    As,
    Break,
    Comptime,
    Const,
    Continue,
    Else,
    Entry,
    Enum,
    Extern,
    False,
    Fn,
    If,
    Let,
    Loop,
    Match,
    Mut,
    OffsetOf,
    Panic,
    Return,
    SizeOf,
    Static,
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
    /// `::`, between an enum and its variant (ADR 0035).
    ColonColon,
    /// `=>`, between a pattern and its arm's expression (ADR 0037).
    FatArrow,
    /// `_`, the wildcard pattern, and never a name (ADR 0037).
    Underscore,
    Comma,
    Semi,
    Dot,
    /// `?`, after an `Option` or a `Result`, which passes its failure on
    /// (ADR 0046).
    Question,

    /// `#`, which starts an attribute.
    Pound,

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
            "comptime" => TokenKind::Comptime,
            "const" => TokenKind::Const,
            "continue" => TokenKind::Continue,
            "else" => TokenKind::Else,
            "entry" => TokenKind::Entry,
            "enum" => TokenKind::Enum,
            "extern" => TokenKind::Extern,
            "false" => TokenKind::False,
            "fn" => TokenKind::Fn,
            "if" => TokenKind::If,
            "let" => TokenKind::Let,
            "loop" => TokenKind::Loop,
            "match" => TokenKind::Match,
            "mut" => TokenKind::Mut,
            "offset_of" => TokenKind::OffsetOf,
            "panic" => TokenKind::Panic,
            "return" => TokenKind::Return,
            "size_of" => TokenKind::SizeOf,
            "static" => TokenKind::Static,
            "struct" => TokenKind::Struct,
            "true" => TokenKind::True,
            "volatile" => TokenKind::Volatile,
            "while" => TokenKind::While,
            "_" => TokenKind::Underscore,
            _ => return None,
        })
    }

    /// How the token is named in diagnostics.
    pub fn describe(self) -> &'static str {
        match self {
            TokenKind::Ident => "identifier",
            TokenKind::Int => "integer literal",
            TokenKind::Byte => "byte literal",
            TokenKind::ByteString => "byte string",
            TokenKind::AlignOf => "`align_of`",
            TokenKind::As => "`as`",
            TokenKind::Break => "`break`",
            TokenKind::Comptime => "`comptime`",
            TokenKind::Const => "`const`",
            TokenKind::Continue => "`continue`",
            TokenKind::Else => "`else`",
            TokenKind::Entry => "`entry`",
            TokenKind::Enum => "`enum`",
            TokenKind::Extern => "`extern`",
            TokenKind::False => "`false`",
            TokenKind::Fn => "`fn`",
            TokenKind::If => "`if`",
            TokenKind::Let => "`let`",
            TokenKind::Loop => "`loop`",
            TokenKind::Match => "`match`",
            TokenKind::Mut => "`mut`",
            TokenKind::OffsetOf => "`offset_of`",
            TokenKind::Panic => "`panic`",
            TokenKind::Return => "`return`",
            TokenKind::SizeOf => "`size_of`",
            TokenKind::Static => "`static`",
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
            TokenKind::ColonColon => "`::`",
            TokenKind::FatArrow => "`=>`",
            TokenKind::Underscore => "`_`",
            TokenKind::Comma => "`,`",
            TokenKind::Semi => "`;`",
            TokenKind::Dot => "`.`",
            TokenKind::Question => "`?`",
            TokenKind::Pound => "`#`",
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
