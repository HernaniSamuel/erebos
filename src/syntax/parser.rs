//! Recursive-descent parser (ADR 0004). Errors are reported and the parser
//! resynchronizes at the next item, so one run reports several errors.

use crate::diag::{Diagnostic, Diagnostics};
use crate::span::Span;
use crate::syntax::ast::{
    Attr, BinOp, Block, ComptimeDecl, ConstDecl, Expr, ExprKind, FieldDecl, FieldInit, File,
    FnDecl, FnKind, Ident, Item, Let, LogicalOp, Param, Stmt, StructDecl, Ty, TyKind, UnOp,
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
        no_struct: false,
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
    /// Set while parsing the condition of `if` or `while`, where `{` opens
    /// the body: `if x == y { ... }` is not a struct literal `y { ... }`.
    no_struct: bool,
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
            TokenKind::Entry
                | TokenKind::Panic
                | TokenKind::Fn
                | TokenKind::Extern
                | TokenKind::Struct
                | TokenKind::Const
                | TokenKind::Comptime
                | TokenKind::Pound
                | TokenKind::Eof
        ) {
            self.bump();
        }
    }

    fn item(&mut self) -> PResult<Item> {
        let mut attrs = Vec::new();
        while self.at(TokenKind::Pound) {
            attrs.push(self.attr()?);
        }
        match self.peek().kind {
            TokenKind::Entry | TokenKind::Panic | TokenKind::Fn | TokenKind::Extern => {
                Ok(Item::Fn(self.fn_decl(attrs)?))
            }
            TokenKind::Struct => Ok(Item::Struct(self.struct_decl(attrs)?)),
            TokenKind::Const => Ok(Item::Const(self.const_decl(attrs)?)),
            TokenKind::Comptime => {
                let start = self.bump().span;
                let block = self.block()?;
                Ok(Item::Comptime(ComptimeDecl {
                    attrs,
                    span: start.to(block.span),
                    block,
                }))
            }
            _ => {
                // A stray token, not a missing one: point at the token itself.
                let err = self.report_unexpected("an item", self.peek().span);
                // Guarantee progress before resynchronizing.
                self.bump();
                Err(err)
            }
        }
    }

    /// `#[name]` or `#[name(N)]`.
    fn attr(&mut self) -> PResult<Attr> {
        let pound = self.bump();
        self.expect(TokenKind::LBracket)?;
        let name = self.ident()?;
        let arg = if self.eat(TokenKind::LParen).is_some() {
            let token = self.peek();
            if token.kind != TokenKind::Int {
                return Err(self.error_expected("an integer literal"));
            }
            self.bump();
            let value = lexer::int_value(&self.text[token.span.range()]).ok();
            self.expect(TokenKind::RParen)?;
            Some((value, token.span))
        } else {
            None
        };
        let close = self.expect(TokenKind::RBracket)?;
        Ok(Attr {
            name,
            arg,
            span: pound.span.to(close.span),
        })
    }

    fn fn_decl(&mut self, attrs: Vec<Attr>) -> PResult<FnDecl> {
        let start = self.peek().span;
        let external = self.eat(TokenKind::Extern).is_some();
        let kind = if external {
            FnKind::Plain
        } else if self.eat(TokenKind::Entry).is_some() {
            FnKind::Entry
        } else if self.eat(TokenKind::Panic).is_some() {
            FnKind::Panic
        } else {
            FnKind::Plain
        };
        self.expect(TokenKind::Fn)?;
        let name = self.ident()?;
        self.expect(TokenKind::LParen)?;
        let params = self.comma_list(Self::param)?;
        let ret = match self.eat(TokenKind::Arrow) {
            Some(_) => Some(self.ty()?),
            None => None,
        };
        if external {
            let semi = self.expect(TokenKind::Semi)?;
            return Ok(FnDecl {
                attrs,
                kind,
                name,
                params,
                ret,
                span: start.to(semi.span),
                body: None,
            });
        }
        let body = self.block()?;
        Ok(FnDecl {
            attrs,
            kind,
            name,
            params,
            ret,
            span: start.to(body.span),
            body: Some(body),
        })
    }

    fn const_decl(&mut self, attrs: Vec<Attr>) -> PResult<ConstDecl> {
        let start = self.bump().span;
        let name = self.ident()?;
        self.expect(TokenKind::Colon)?;
        let ty = self.ty()?;
        self.expect(TokenKind::Eq)?;
        let value = self.expr()?;
        let semi = self.expect(TokenKind::Semi)?;
        Ok(ConstDecl {
            attrs,
            name,
            ty,
            value,
            span: start.to(semi.span),
        })
    }

    fn struct_decl(&mut self, attrs: Vec<Attr>) -> PResult<StructDecl> {
        let start = self.bump().span;
        let name = self.ident()?;
        self.expect(TokenKind::LBrace)?;
        let fields = self.brace_list(|p| {
            let name = p.ident()?;
            p.expect(TokenKind::Colon)?;
            let ty = p.ty()?;
            Ok(FieldDecl { name, ty })
        })?;
        Ok(StructDecl {
            attrs,
            name,
            fields,
            span: start.to(self.tokens[self.pos - 1].span),
        })
    }

    /// Parses `item, item, ...}` after an opening `{`, allowing a trailing
    /// comma, and consumes the `}`.
    fn brace_list<T>(&mut self, mut item: impl FnMut(&mut Self) -> PResult<T>) -> PResult<Vec<T>> {
        let mut items = Vec::new();
        while !self.at(TokenKind::RBrace) {
            items.push(item(self)?);
            if self.eat(TokenKind::Comma).is_none() {
                break;
            }
        }
        self.expect(TokenKind::RBrace)?;
        Ok(items)
    }

    fn param(&mut self) -> PResult<Param> {
        let mutable = self.eat(TokenKind::Mut).is_some();
        let name = self.ident()?;
        self.expect(TokenKind::Colon)?;
        let ty = self.ty()?;
        Ok(Param { mutable, name, ty })
    }

    /// Parses `item, item, ...)` after an opening `(`, allowing a trailing
    /// comma, and consumes the `)`.
    fn comma_list<T>(&mut self, mut item: impl FnMut(&mut Self) -> PResult<T>) -> PResult<Vec<T>> {
        let mut items = Vec::new();
        while !self.at(TokenKind::RParen) {
            items.push(item(self)?);
            if self.eat(TokenKind::Comma).is_none() {
                break;
            }
        }
        self.expect(TokenKind::RParen)?;
        Ok(items)
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
            TokenKind::LBracket => {
                self.bump();
                let elem = Box::new(self.ty()?);
                self.expect(TokenKind::Semi)?;
                let len = self.array_len()?;
                let close = self.expect(TokenKind::RBracket)?;
                return Ok(Ty {
                    kind: TyKind::Array { elem, len },
                    span: token.span.to(close.span),
                });
            }
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
        let saved = std::mem::replace(&mut self.no_struct, false);
        let block = self.block_inner();
        self.no_struct = saved;
        block
    }

    fn block_inner(&mut self) -> PResult<Block> {
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
                TokenKind::Const => {
                    let token = self.peek();
                    self.diags.push(
                        Diagnostic::error(
                            "parser::local-const",
                            "`const` items cannot be declared inside a function",
                        )
                        .label(token.span, "inside a function")
                        .help("compute the value at compile time with `let name: T = comptime ...;`, or move the `const` out of the function (ADR 0029)"),
                    );
                    // Consumed, so that the rest of the block parses.
                    self.const_decl(Vec::new())?;
                }
                _ => {
                    // As in Rust, a statement starting with a block-like
                    // expression ends with it: `if c { } - 1` is not a
                    // subtraction.
                    let expr = if self.at_block_like() {
                        self.primary()?
                    } else {
                        self.expr()?
                    };
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

    fn at_block_like(&self) -> bool {
        matches!(
            self.peek().kind,
            TokenKind::LBrace | TokenKind::If | TokenKind::While | TokenKind::Loop
        )
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

    /// An expression inside delimiters, where struct literals are allowed
    /// again even within a condition.
    fn nested_expr(&mut self) -> PResult<Expr> {
        let saved = std::mem::replace(&mut self.no_struct, false);
        let expr = self.expr();
        self.no_struct = saved;
        expr
    }

    /// The condition of `if` or `while`, which a `{` ends.
    fn condition(&mut self) -> PResult<Expr> {
        let saved = std::mem::replace(&mut self.no_struct, true);
        let expr = self.expr();
        self.no_struct = saved;
        expr
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
                Infix::Logical(op) => ExprKind::Logical(op, l, r),
                Infix::Assign => ExprKind::Assign(l, r),
                Infix::CompoundAssign(op) => ExprKind::CompoundAssign(op, l, r),
            };
            lhs = Expr { kind, span };

            // `a < b < c` compiles in C and is almost always a bug.
            if left_bp == COMPARE_BP
                && let Some((COMPARE_BP, _, _)) = infix(self.peek().kind)
            {
                let next = self.peek();
                self.diags.push(
                    Diagnostic::error(
                        "parser::chained-comparison",
                        "comparison operators cannot be chained",
                    )
                    .label(lhs.span.to(next.span), "chained here")
                    .help("compare twice and combine: `a < b && b < c`"),
                );
                return Err(Reported);
            }
        }
        Ok(lhs)
    }

    /// Prefix operators, then a primary expression with its postfix
    /// operators.
    fn prefix(&mut self) -> PResult<Expr> {
        let token = self.peek();
        let op = match token.kind {
            TokenKind::Minus => Some(UnOp::Neg),
            TokenKind::Bang => Some(UnOp::Not),
            TokenKind::Star => None,
            TokenKind::Amp => return self.addr_of(),
            _ => return self.postfix(),
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

    /// `&raw const place` or `&raw mut place`. A plain `&` is left for
    /// references (M5), so that its meaning never changes (ADR 0021).
    fn addr_of(&mut self) -> PResult<Expr> {
        let amp = self.bump();
        let raw = self.peek();
        if raw.kind != TokenKind::Ident || &self.text[raw.span.range()] != "raw" {
            let found = raw.kind.describe();
            self.diags.push(
                Diagnostic::error(
                    "parser::unexpected-token",
                    format!("expected `raw`, found {found}"),
                )
                .label(self.missing_token_span(), "expected `raw`")
                .help("addresses are taken with `&raw const place` or `&raw mut place`; `&` alone is reserved for references (ADR 0021)"),
            );
            return Err(Reported);
        }
        self.bump();
        let mutable = match self.peek().kind {
            TokenKind::Mut => true,
            TokenKind::Const => false,
            _ => return Err(self.error_expected("`const` or `mut`")),
        };
        self.bump();
        let place = Box::new(self.expr_bp(PREFIX_BP)?);
        Ok(Expr {
            span: amp.span.to(place.span),
            kind: ExprKind::AddrOf { mutable, place },
        })
    }

    /// A primary expression followed by any indexing and field accesses.
    fn postfix(&mut self) -> PResult<Expr> {
        let mut expr = self.primary()?;
        loop {
            if self.eat(TokenKind::LBracket).is_some() {
                let index = self.nested_expr()?;
                let close = self.expect(TokenKind::RBracket)?;
                expr = Expr {
                    span: expr.span.to(close.span),
                    kind: ExprKind::Index(Box::new(expr), Box::new(index)),
                };
            } else if self.eat(TokenKind::Dot).is_some() {
                let field = self.ident()?;
                expr = Expr {
                    span: expr.span.to(field.span),
                    kind: ExprKind::Field(Box::new(expr), field),
                };
            } else {
                return Ok(expr);
            }
        }
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
            TokenKind::True | TokenKind::False => {
                self.bump();
                ExprKind::Bool(token.kind == TokenKind::True)
            }
            TokenKind::Continue => {
                self.bump();
                ExprKind::Continue
            }
            // Like `return`, it covers the whole expression to its right
            // (ADR 0029).
            TokenKind::Comptime => {
                self.bump();
                let value = self.expr()?;
                return Ok(Expr {
                    span: token.span.to(value.span),
                    kind: ExprKind::Comptime(Box::new(value)),
                });
            }
            TokenKind::Break | TokenKind::Return => {
                self.bump();
                let value = if self.at_expr_end() {
                    None
                } else {
                    Some(Box::new(self.expr()?))
                };
                let span = value.as_ref().map_or(token.span, |v| token.span.to(v.span));
                let kind = if token.kind == TokenKind::Break {
                    ExprKind::Break(value)
                } else {
                    ExprKind::Return(value)
                };
                return Ok(Expr { kind, span });
            }
            TokenKind::If => return self.if_expr(),
            TokenKind::While => {
                self.bump();
                let cond = Box::new(self.condition()?);
                let body = self.block()?;
                return Ok(Expr {
                    span: token.span.to(body.span),
                    kind: ExprKind::While { cond, body },
                });
            }
            TokenKind::Ident => {
                let name = self.ident()?;
                if self.eat(TokenKind::LParen).is_some() {
                    let args = self.comma_list(Self::nested_expr)?;
                    let end = self.tokens[self.pos - 1].span;
                    return Ok(Expr {
                        span: token.span.to(end),
                        kind: ExprKind::Call(name, args),
                    });
                }
                if !self.no_struct && self.eat(TokenKind::LBrace).is_some() {
                    let fields = self.brace_list(|p| {
                        let name = p.ident()?;
                        p.expect(TokenKind::Colon)?;
                        let value = p.nested_expr()?;
                        Ok(FieldInit { name, value })
                    })?;
                    let end = self.tokens[self.pos - 1].span;
                    return Ok(Expr {
                        span: token.span.to(end),
                        kind: ExprKind::StructLit(name, fields),
                    });
                }
                ExprKind::Name(name)
            }
            TokenKind::LParen => {
                self.bump();
                if let Some(close) = self.eat(TokenKind::RParen) {
                    return Ok(Expr {
                        kind: ExprKind::Unit,
                        span: token.span.to(close.span),
                    });
                }
                let inner = self.nested_expr()?;
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
            TokenKind::LBracket => return self.array(),
            TokenKind::SizeOf | TokenKind::AlignOf | TokenKind::OffsetOf => {
                return self.layout_query();
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

impl Parser<'_> {
    /// Whether the next token ends an expression, so `break`/`return` have
    /// no value.
    fn at_expr_end(&self) -> bool {
        matches!(
            self.peek().kind,
            TokenKind::Semi
                | TokenKind::RBrace
                | TokenKind::RParen
                | TokenKind::RBracket
                | TokenKind::Eof
        )
    }

    /// `size_of(T)`, `align_of(T)` or `offset_of(T, field)`: their
    /// argument is a type, not an expression (ADR 0022).
    fn layout_query(&mut self) -> PResult<Expr> {
        let keyword = self.bump();
        self.expect(TokenKind::LParen)?;
        let ty = self.ty()?;
        let kind = match keyword.kind {
            TokenKind::SizeOf => ExprKind::SizeOf(ty),
            TokenKind::AlignOf => ExprKind::AlignOf(ty),
            _ => {
                self.expect(TokenKind::Comma)?;
                let field = self.ident()?;
                ExprKind::OffsetOf(ty, field)
            }
        };
        let close = self.expect(TokenKind::RParen)?;
        Ok(Expr {
            kind,
            span: keyword.span.to(close.span),
        })
    }

    /// The length of an array type or repeat expression: an integer
    /// literal (constant expressions come with `const`, M2.3).
    fn array_len(&mut self) -> PResult<Option<u64>> {
        let token = self.peek();
        if token.kind != TokenKind::Int {
            return Err(self.error_expected("an integer literal for the length"));
        }
        self.bump();
        Ok(lexer::int_value(&self.text[token.span.range()]).ok())
    }

    /// `[a, b, c]` or `[value; N]` (ADR 0021).
    fn array(&mut self) -> PResult<Expr> {
        let open = self.bump();
        let mut elems = Vec::new();
        if !self.at(TokenKind::RBracket) {
            let first = self.nested_expr()?;
            if self.eat(TokenKind::Semi).is_some() {
                let len = self.array_len()?;
                let close = self.expect(TokenKind::RBracket)?;
                return Ok(Expr {
                    kind: ExprKind::Repeat(Box::new(first), len),
                    span: open.span.to(close.span),
                });
            }
            elems.push(first);
            while self.eat(TokenKind::Comma).is_some() && !self.at(TokenKind::RBracket) {
                elems.push(self.nested_expr()?);
            }
        }
        let close = self.expect(TokenKind::RBracket)?;
        Ok(Expr {
            kind: ExprKind::Array(elems),
            span: open.span.to(close.span),
        })
    }

    fn if_expr(&mut self) -> PResult<Expr> {
        let start = self.bump().span;
        let cond = Box::new(self.condition()?);
        let then = self.block()?;
        let mut span = start.to(then.span);
        let else_ = if self.eat(TokenKind::Else).is_some() {
            let else_ = if self.at(TokenKind::If) {
                self.if_expr()?
            } else {
                let block = self.block()?;
                Expr {
                    span: block.span,
                    kind: ExprKind::Block(block),
                }
            };
            span = span.to(else_.span);
            Some(Box::new(else_))
        } else {
            None
        };
        Ok(Expr {
            kind: ExprKind::If { cond, then, else_ },
            span,
        })
    }
}

enum Infix {
    Binary(BinOp),
    Logical(LogicalOp),
    Assign,
    CompoundAssign(BinOp),
}

/// Binding powers, loosest first, following Rust's precedence. A left power
/// lower than the right one makes an operator left-associative; assignment
/// is right-associative.
const ASSIGN_BP: (u8, u8) = (2, 1);
const COMPARE_BP: u8 = 7;
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
        TokenKind::SlashEq => (l, r, CompoundAssign(BinOp::Div)),
        TokenKind::PercentEq => (l, r, CompoundAssign(BinOp::Rem)),
        TokenKind::AmpEq => (l, r, CompoundAssign(BinOp::BitAnd)),
        TokenKind::PipeEq => (l, r, CompoundAssign(BinOp::BitOr)),
        TokenKind::CaretEq => (l, r, CompoundAssign(BinOp::BitXor)),
        TokenKind::ShlEq => (l, r, CompoundAssign(BinOp::Shl)),
        TokenKind::ShrEq => (l, r, CompoundAssign(BinOp::Shr)),
        TokenKind::OrOr => (3, 4, Infix::Logical(LogicalOp::Or)),
        TokenKind::AndAnd => (5, 6, Infix::Logical(LogicalOp::And)),
        TokenKind::EqEq => (COMPARE_BP, 8, Binary(BinOp::Eq)),
        TokenKind::Ne => (COMPARE_BP, 8, Binary(BinOp::Ne)),
        TokenKind::Lt => (COMPARE_BP, 8, Binary(BinOp::Lt)),
        TokenKind::Le => (COMPARE_BP, 8, Binary(BinOp::Le)),
        TokenKind::Gt => (COMPARE_BP, 8, Binary(BinOp::Gt)),
        TokenKind::Ge => (COMPARE_BP, 8, Binary(BinOp::Ge)),
        TokenKind::Pipe => (9, 10, Binary(BinOp::BitOr)),
        TokenKind::Caret => (11, 12, Binary(BinOp::BitXor)),
        TokenKind::Amp => (13, 14, Binary(BinOp::BitAnd)),
        TokenKind::Shl => (15, 16, Binary(BinOp::Shl)),
        TokenKind::Shr => (15, 16, Binary(BinOp::Shr)),
        TokenKind::Plus => (17, 18, Binary(BinOp::Add)),
        TokenKind::Minus => (17, 18, Binary(BinOp::Sub)),
        TokenKind::Star => (19, 20, Binary(BinOp::Mul)),
        TokenKind::Slash => (19, 20, Binary(BinOp::Div)),
        TokenKind::Percent => (19, 20, Binary(BinOp::Rem)),
        _ => return None,
    })
}
