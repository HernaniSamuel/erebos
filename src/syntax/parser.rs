//! Recursive-descent parser (ADR 0004). Errors are reported and the parser
//! resynchronizes at the next item, so one run reports several errors.

use crate::diag::{Diagnostic, Diagnostics};
use crate::span::Span;
use crate::syntax::ast::{Block, Expr, ExprKind, File, FnDecl, Ident, Item, Stmt, Ty, TyKind};
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
            TokenKind::Entry | TokenKind::Fn | TokenKind::Eof
        ) {
            self.bump();
        }
    }

    fn item(&mut self) -> PResult<Item> {
        match self.peek().kind {
            TokenKind::Entry | TokenKind::Fn => Ok(Item::Fn(self.fn_decl()?)),
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
        let is_entry = self.eat(TokenKind::Entry).is_some();
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
            is_entry,
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
        match self.eat(TokenKind::Bang) {
            Some(token) => Ok(Ty {
                kind: TyKind::Never,
                span: token.span,
            }),
            None => Err(self.error_expected("a type")),
        }
    }

    fn block(&mut self) -> PResult<Block> {
        let open = self.expect(TokenKind::LBrace)?;
        let mut stmts = Vec::new();
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
                _ => stmts.push(Stmt::Expr(self.expr()?)),
            }
        }
        let close = self.bump();
        Ok(Block {
            stmts,
            span: open.span.to(close.span),
        })
    }

    fn expr(&mut self) -> PResult<Expr> {
        match self.peek().kind {
            TokenKind::Loop => {
                let start = self.bump().span;
                let body = self.block()?;
                Ok(Expr {
                    span: start.to(body.span),
                    kind: ExprKind::Loop(body),
                })
            }
            _ => Err(self.error_expected("an expression")),
        }
    }
}
