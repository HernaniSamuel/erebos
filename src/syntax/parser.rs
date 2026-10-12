//! Recursive-descent parser (ADR 0004). Errors are reported and the parser
//! resynchronizes at the next item, so one run reports several errors.

use crate::diag::{Diagnostic, Diagnostics};
use crate::span::Span;
use crate::syntax::ast::{
    BinOp, Block, Expr, ExprKind, File, FnDecl, FnKind, Ident, Item, Let, Stmt, Ty, TyKind, UnOp,
};
use crate::syntax::lexer;
use crate::syntax::token::{Token, TokenKind};

/// `tokens` must end with `Eof`, as produced by the lexer.
pub fn parse(text: &str, tokens: &[Token], diags: &mut Diagnostics) -> File {
    let mut parser = Parser {
        text,
        tokens,
        pos: 0,
        diags,
    };
    parser.file()
}

/// The error has already been reported; the caller only needs to recover.
struct Reported;

type PResult<T> = Result<T, Reported>;

struct Parser<'a> {
    text: &'a str,
    tokens: &'a [Token],
    pos: usize,
    diags: &'a mut Diagnostics,
}

impl Parser<'_> {
    fn peek(&self) -> Token {
        self.tokens[self.pos]
    }

    fn at(&self, kind: TokenKind) -> bool {
        self.peek().kind == kind
    }

    fn bump(&mut self) -> Token {
        let token = self.peek();
        if token.kind != TokenKind::Eof {
            self.pos += 1;
        }
        token
    }

    fn eat(&mut self, kind: TokenKind) -> Option<Token> {
        self.at(kind).then(|| self.bump())
    }

    fn expect(&mut self, kind: TokenKind) -> PResult<Token> {
        match self.eat(kind) {
            Some(token) => Ok(token),
            None => Err(self.error_expected(kind.describe())),
        }
    }

    /// Reports that `what` is missing before the next token.
    fn error_expected(&mut self, what: &str) -> Reported {
        let span = self.missing_token_span();
        self.report_unexpected(what, span)
    }

    fn report_unexpected(&mut self, what: &str, span: Span) -> Reported {
        let found = self.peek();
        self.diags.push(
            Diagnostic::error(
                "parser::unexpected-token",
                format!("expected {what}, found {}", found.kind.describe()),
            )
            .label(span, format!("expected {what}")),
        );
        Reported
    }

    /// Where a missing token should be reported: on the next token if it is
    /// on the same line, otherwise right after the previous token, which is
    /// where the programmer has to type it.
    fn missing_token_span(&self) -> Span {
        let found = self.peek();
        let Some(prev) = self.pos.checked_sub(1).map(|i| self.tokens[i]) else {
            return found.span;
        };
        let gap = &self.text[prev.span.hi as usize..found.span.lo as usize];
        if found.kind == TokenKind::Eof || gap.contains('\n') {
            Span::new(prev.span.file, prev.span.hi as usize, prev.span.hi as usize)
        } else {
            found.span
        }
    }

    fn file(&mut self) -> File {
        let mut items = Vec::new();
        while !self.at(TokenKind::Eof) {
            match self.item() {
                Ok(item) => items.push(item),
                Err(Reported) => self.recover_to_item(),
            }
        }
        File { items }
    }

    /// Skips to the next token that can start an item.
    fn recover_to_item(&mut self) {
        while !matches!(
            self.peek().kind,
            TokenKind::Entry | TokenKind::Panic | TokenKind::Fn | TokenKind::Eof
        ) {
            self.bump();
        }
    }

    fn item(&mut self) -> PResult<Item> {
        match self.peek().kind {
            TokenKind::Entry | TokenKind::Panic | TokenKind::Fn => Ok(Item::Fn(self.fn_decl()?)),
            _ => {
                // A stray token, not a missing one: point at the token itself.
                let err = self.report_unexpected("an item", self.peek().span);
                // Guarantee progress before resynchronizing.
                self.bump();
                Err(err)
            }
        }
    }

    fn fn_decl(&mut self) -> PResult<FnDecl> {
        let start = self.peek().span;
        let kind = if self.eat(TokenKind::Entry).is_some() {
            FnKind::Entry
        } else if self.eat(TokenKind::Panic).is_some() {
            FnKind::Panic
        } else {
            FnKind::Plain
        };
        self.expect(TokenKind::Fn)?;
        let name = self.ident()?;
        self.expect(TokenKind::LParen)?;
        self.expect(TokenKind::RParen)?;
        let ret = match self.eat(TokenKind::Arrow) {
            Some(_) => Some(self.ty()?),
            None => None,
        };
        let body = self.block()?;
        Ok(FnDecl {
            kind,
            name,
            ret,
            span: start.to(body.span),
            body,
        })
    }

    fn ident(&mut self) -> PResult<Ident> {
        let token = self.expect(TokenKind::Ident)?;
        Ok(Ident {
            name: self.text[token.span.range()].to_owned(),
            span: token.span,
        })
    }

    fn ty(&mut self) -> PResult<Ty> {
        let token = self.peek();
        let kind = match token.kind {
            TokenKind::Bang => {
                self.bump();
                TyKind::Never
            }
            TokenKind::LParen => {
                self.bump();
                let close = self.expect(TokenKind::RParen)?;
                return Ok(Ty {
                    kind: TyKind::Unit,
                    span: token.span.to(close.span),
                });
            }
            TokenKind::Ident => TyKind::Named(self.ident()?),
            TokenKind::Star => {
                self.bump();
                let mutable = match self.peek().kind {
                    TokenKind::Mut => true,
                    TokenKind::Const => false,
                    _ => return Err(self.error_expected("`const` or `mut`")),
                };
                self.bump();
                let volatile = self.eat(TokenKind::Volatile).is_some();
                let pointee = self.ty()?;
                return Ok(Ty {
                    span: token.span.to(pointee.span),
                    kind: TyKind::Ptr {
                        mutable,
                        volatile,
                        pointee: Box::new(pointee),
                    },
                });
            }
            _ => return Err(self.error_expected("a type")),
        };
        Ok(Ty {
            kind,
            span: token.span,
        })
    }

    fn block(&mut self) -> PResult<Block> {
        let open = self.expect(TokenKind::LBrace)?;
        let mut stmts = Vec::new();
        let mut tail = None;
        loop {
            match self.peek().kind {
                TokenKind::RBrace => break,
                TokenKind::Eof => {
                    self.diags.push(
                        Diagnostic::error("parser::unclosed-block", "unclosed block")
                            .label(open.span, "this `{` is never closed")
                            .label(self.missing_token_span(), "expected `}`"),
                    );
                    return Err(Reported);
                }
                TokenKind::Semi => {
                    // An empty statement.
                    self.bump();
                }
                TokenKind::Let => stmts.push(Stmt::Let(self.let_stmt()?)),
                _ => {
                    let expr = self.expr()?;
                    if let Some(semi) = self.eat(TokenKind::Semi) {
                        stmts.push(Stmt::Expr {
                            expr,
                            semi: Some(semi.span),
                        });
                    } else if self.at(TokenKind::RBrace) {
                        tail = Some(Box::new(expr));
                    } else if expr.kind.is_block_like() {
                        stmts.push(Stmt::Expr { expr, semi: None });
                    } else {
                        return Err(self.error_expected("`;`"));
                    }
                }
            }
        }
        let close = self.bump();
        Ok(Block {
            stmts,
            tail,
            span: open.span.to(close.span),
        })
    }

    fn let_stmt(&mut self) -> PResult<Let> {
        let start = self.bump().span;
        let mutable = self.eat(TokenKind::Mut).is_some();
        let name = self.ident()?;
        if !self.at(TokenKind::Colon) {
            let found = self.peek();
            self.diags.push(
                Diagnostic::error(
                    "parser::let-without-type",
                    format!("expected `:`, found {}", found.kind.describe()),
                )
                .label(self.missing_token_span(), "expected `: Type`")
                .help(format!(
                    "every `let` states its type, e.g. `let {}: u32 = ...` (ADR 0008)",
                    name.name
                )),
            );
            return Err(Reported);
        }
        self.bump();
        let ty = self.ty()?;
        self.expect(TokenKind::Eq)?;
        let init = self.expr()?;
        let semi = self.expect(TokenKind::Semi)?;
        Ok(Let {
            mutable,
            name,
            ty,
            init,
            span: start.to(semi.span),
        })
    }

    fn expr(&mut self) -> PResult<Expr> {
        self.expr_bp(0)
    }

    /// Pratt parser: parses an expression whose operators all bind at least
    /// as tightly as `min_bp`.
    fn expr_bp(&mut self, min_bp: u8) -> PResult<Expr> {
        let mut lhs = self.prefix()?;
        loop {
            let token = self.peek();

            if token.kind == TokenKind::As {
                if AS_BP < min_bp {
                    break;
                }
                self.bump();
                let ty = self.ty()?;
                lhs = Expr {
                    span: lhs.span.to(ty.span),
                    kind: ExprKind::Cast(Box::new(lhs), ty),
                };
                continue;
            }

            let Some((left_bp, right_bp, op)) = infix(token.kind) else {
                break;
            };
            if left_bp < min_bp {
                break;
            }
            self.bump();
            let rhs = self.expr_bp(right_bp)?;
            let span = lhs.span.to(rhs.span);
            let (l, r) = (Box::new(lhs), Box::new(rhs));
            let kind = match op {
                Infix::Binary(op) => ExprKind::Binary(op, l, r),
                Infix::Assign => ExprKind::Assign(l, r),
                Infix::CompoundAssign(op) => ExprKind::CompoundAssign(op, l, r),
            };
            lhs = Expr { kind, span };
        }
        Ok(lhs)
    }

    /// Prefix operators, then a primary expression.
    fn prefix(&mut self) -> PResult<Expr> {
        let token = self.peek();
        let op = match token.kind {
            TokenKind::Minus => Some(UnOp::Neg),
            TokenKind::Bang => Some(UnOp::Not),
            TokenKind::Star => None,
            _ => return self.primary(),
        };
        self.bump();
        let operand = Box::new(self.expr_bp(PREFIX_BP)?);
        Ok(Expr {
            span: token.span.to(operand.span),
            kind: match op {
                Some(op) => ExprKind::Unary(op, operand),
                None => ExprKind::Deref(operand),
            },
        })
    }

    fn primary(&mut self) -> PResult<Expr> {
        let token = self.peek();
        let text = &self.text[token.span.range()];
        let kind = match token.kind {
            TokenKind::Int => {
                self.bump();
                ExprKind::Int(lexer::int_value(text).ok())
            }
            TokenKind::Byte => {
                self.bump();
                ExprKind::Byte(lexer::byte_value(text).ok())
            }
            TokenKind::Ident => ExprKind::Name(self.ident()?),
            TokenKind::LParen => {
                self.bump();
                if let Some(close) = self.eat(TokenKind::RParen) {
                    return Ok(Expr {
                        kind: ExprKind::Unit,
                        span: token.span.to(close.span),
                    });
                }
                let inner = self.expr()?;
                let close = self.expect(TokenKind::RParen)?;
                // Parentheses only group; widen the span to what was written.
                return Ok(Expr {
                    kind: inner.kind,
                    span: token.span.to(close.span),
                });
            }
            TokenKind::LBrace => {
                let block = self.block()?;
                return Ok(Expr {
                    span: block.span,
                    kind: ExprKind::Block(block),
                });
            }
            TokenKind::Loop => {
                self.bump();
                let body = self.block()?;
                return Ok(Expr {
                    span: token.span.to(body.span),
                    kind: ExprKind::Loop(body),
                });
            }
            _ => return Err(self.error_expected("an expression")),
        };
        Ok(Expr {
            kind,
            span: token.span,
        })
    }
}

enum Infix {
    Binary(BinOp),
    Assign,
    CompoundAssign(BinOp),
}

/// Binding powers, loosest first, following Rust's precedence. A left power
/// lower than the right one makes an operator left-associative; assignment
/// is right-associative. Gaps are left for comparisons and logical operators.
const ASSIGN_BP: (u8, u8) = (2, 1);
const AS_BP: u8 = 21;
const PREFIX_BP: u8 = 23;

fn infix(kind: TokenKind) -> Option<(u8, u8, Infix)> {
    use Infix::{Assign, Binary, CompoundAssign};
    let (l, r) = ASSIGN_BP;
    Some(match kind {
        TokenKind::Eq => (l, r, Assign),
        TokenKind::PlusEq => (l, r, CompoundAssign(BinOp::Add)),
        TokenKind::MinusEq => (l, r, CompoundAssign(BinOp::Sub)),
        TokenKind::StarEq => (l, r, CompoundAssign(BinOp::Mul)),
        TokenKind::AmpEq => (l, r, CompoundAssign(BinOp::BitAnd)),
        TokenKind::PipeEq => (l, r, CompoundAssign(BinOp::BitOr)),
        TokenKind::CaretEq => (l, r, CompoundAssign(BinOp::BitXor)),
        TokenKind::ShlEq => (l, r, CompoundAssign(BinOp::Shl)),
        TokenKind::ShrEq => (l, r, CompoundAssign(BinOp::Shr)),
        TokenKind::Pipe => (9, 10, Binary(BinOp::BitOr)),
        TokenKind::Caret => (11, 12, Binary(BinOp::BitXor)),
        TokenKind::Amp => (13, 14, Binary(BinOp::BitAnd)),
        TokenKind::Shl => (15, 16, Binary(BinOp::Shl)),
        TokenKind::Shr => (15, 16, Binary(BinOp::Shr)),
        TokenKind::Plus => (17, 18, Binary(BinOp::Add)),
        TokenKind::Minus => (17, 18, Binary(BinOp::Sub)),
        TokenKind::Star => (19, 20, Binary(BinOp::Mul)),
        _ => return None,
    })
}
