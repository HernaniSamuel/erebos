//! Recursive-descent parser (ADR 0004). Errors are reported and the parser
//! resynchronizes at the next item, so one run reports several errors.

use crate::builtin::{builtin_variant_help, is_builtin_enum};
use crate::diag::{Diagnostic, Diagnostics};
use crate::span::Span;
use crate::syntax::ast::{
    AccessMode, Arm, Attr, BinOp, BitFieldDecl, BitStructDecl, Block, ComptimeDecl, ConstDecl,
    EnumDecl, Expr, ExprKind, FieldDecl, FieldInit, File, FnDecl, FnKind, Ident, Item, Let,
    LogicalOp, MemoryDecl, ModDecl, Param, Path, Pattern, PatternKind, PeripheralDecl,
    PeripheralTypeDecl, Placement, PlacementKind, ReadEffect, RegionDecl, RegisterDecl, StaticDecl,
    Stmt, StructDecl, Ty, TyKind, UnOp, UseDecl, UseName, VariantDecl, Visibility, WriteAction,
    WriteEffect,
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
        after_builtin_call: None,
        depth: 0,
        module_depth: None,
    };
    parser.file()
}

/// The help of the errors in a register of a peripheral type (ADR 0058).
const REGISTER_HELP: &str = "a register is written `ACCESS NAME: TYPE at OFFSET`, optionally after `pub`, its access mode `read`, `write` or `read write`, in that order, and its name the identifier just before `:`; registers are separated by `,` (ADR 0058)";

/// The help of the errors after a register's offset (ADR 0060, 0061).
const REGISTER_END_HELP: &str = "after its offset, a register may declare what a read does, `read clears`, `read sets` or `read modifies`, then an alternate group, `in GROUP`, in that order; registers are separated by `,` (ADR 0060, 0061)";

/// The help of the errors in a field of a bit-field struct (ADR 0059,
/// 0060).
const BIT_FIELD_HELP: &str = "a bit field is written `NAME: TYPE at BITS`, optionally after `pub`, then optionally a read effect, `read clears`, `read sets` or `read modifies`, a write effect, `write 0` or `write 1` followed by `clears`, `sets` or `toggles`, and a default, `= VALUE`, in that order; fields are separated by `,` (ADR 0059, 0060)";

/// The help of the errors in the bits of a bit field (ADR 0059).
const BITS_HELP: &str = "a bit field's bits are one bit, as `at 7`, or a range with both ends included, lowest first, as `at 0..=1` (ADR 0059)";

/// The help of the errors at a `..` in a struct literal (ADR 0059).
const REST_HELP: &str = "`..` ends a struct literal and gives the fields it leaves out their defaults; it comes after a `,` when fields come before it, as in `Lcr { dlab: true, .. }` (ADR 0059)";

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
    /// The position of the token after the last call to a function named
    /// `Option` or `Result`, and that name: a `::` there is reported with
    /// the help of `builtin_variant_help`.
    after_builtin_call: Option<(usize, String)>,
    /// How many `{` the tokens consumed so far leave open.
    depth: usize,
    /// Inside the body of a module, the `depth` of its items: a `}` met
    /// there while recovering closes the module.
    module_depth: Option<usize>,
}

impl Parser<'_> {
    fn peek(&self) -> Token {
        self.tokens[self.pos]
    }

    fn at(&self, kind: TokenKind) -> bool {
        self.peek().kind == kind
    }

    /// The kind of the token after the next one.
    fn peek_second(&self) -> TokenKind {
        self.tokens
            .get(self.pos + 1)
            .map_or(TokenKind::Eof, |token| token.kind)
    }

    /// At `static` or `extern static`.
    fn at_static(&self) -> bool {
        self.at(TokenKind::Static)
            || (self.at(TokenKind::Extern) && self.peek_second() == TokenKind::Static)
    }

    /// The text of the token at `pos`, if it is an identifier. The words of
    /// a `memory` block and of peripherals are identifiers, recognized by
    /// their text where they are keywords (ADR 0057, 0058).
    fn word_at(&self, pos: usize) -> Option<&str> {
        let token = self.tokens.get(pos)?;
        (token.kind == TokenKind::Ident).then(|| &self.text[token.span.range()])
    }

    /// At the identifier `word`.
    fn at_word(&self, word: &str) -> bool {
        self.word_at(self.pos) == Some(word)
    }

    /// At `memory {`, which starts a `memory` block where an item starts
    /// (ADR 0057).
    fn at_memory(&self) -> bool {
        self.at_word("memory") && self.peek_second() == TokenKind::LBrace
    }

    /// At a `memory` block in a function body: `memory {` followed by
    /// `region` and a name, or by `code`, `data` or `stack` and `in`.
    /// Anything else after `memory {` there is a struct literal, as
    /// `memory { data: 1 }` (ADR 0057).
    fn at_local_memory(&self) -> bool {
        if !self.at_memory() {
            return false;
        }
        match self.word_at(self.pos + 2) {
            Some("region") => self.word_at(self.pos + 3).is_some(),
            Some("code" | "data" | "stack") => self.word_at(self.pos + 3) == Some("in"),
            _ => false,
        }
    }

    /// At `peripheral` followed by a name, which starts a peripheral type or
    /// a peripheral where an item starts, and is reported inside a function
    /// body, where no expression starts with two names (ADR 0058).
    fn at_peripheral(&self) -> bool {
        self.at_word("peripheral") && self.word_at(self.pos + 1).is_some()
    }

    /// At an access mode word of a register, `read` or `write`: one that the
    /// register's name, the identifier just before `:`, does not follow
    /// (ADR 0058). So `read write: u8` is a `read` register named `write`.
    fn at_access_word(&self, word: &str) -> bool {
        self.at_word(word) && self.peek_second() != TokenKind::Colon
    }

    /// At a `read` that starts a read effect, after a register's offset or
    /// a bit field's bits (ADR 0060): one that does not start the next
    /// register or field, written after a missing `,`. Those are `read:`
    /// (a field named `read`), `read NAME:` (a `read` register) and
    /// `read write` (a `read write` register), so that the missing `,` is
    /// reported, not a missing effect word.
    fn at_read_effect(&self) -> bool {
        let third = self
            .tokens
            .get(self.pos + 2)
            .map_or(TokenKind::Eof, |token| token.kind);
        self.at_word("read")
            && self.peek_second() != TokenKind::Colon
            && third != TokenKind::Colon
            && self.word_at(self.pos + 1) != Some("write")
    }

    fn bump(&mut self) -> Token {
        let token = self.peek();
        match token.kind {
            TokenKind::Eof => return token,
            TokenKind::LBrace => self.depth += 1,
            TokenKind::RBrace => self.depth = self.depth.saturating_sub(1),
            _ => {}
        }
        self.pos += 1;
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

    /// Reports that `what` is missing before the next token, with `help`.
    fn error_expected_help(&mut self, what: &str, help: &str) -> Reported {
        let span = self.missing_token_span();
        self.report_unexpected_help(what, span, help)
    }

    /// `report_unexpected`, with `help`.
    fn report_unexpected_help(&mut self, what: &str, span: Span, help: &str) -> Reported {
        let found = self.peek();
        self.diags.push(
            Diagnostic::error(
                "parser::unexpected-token",
                format!("expected {what}, found {}", found.kind.describe()),
            )
            .label(span, format!("expected {what}"))
            .help(help),
        );
        Reported
    }

    fn report_unexpected(&mut self, what: &str, span: Span) -> Reported {
        let found = self.peek();
        let mut diag = Diagnostic::error(
            "parser::unexpected-token",
            format!("expected {what}, found {}", found.kind.describe()),
        )
        .label(span, format!("expected {what}"));
        // `Option(u32)::None`, a path the variants of `Option` and `Result`
        // do not have (ADR 0045).
        if found.kind == TokenKind::ColonColon
            && let Some((at, name)) = &self.after_builtin_call
            && *at == self.pos
        {
            let variant = self
                .tokens
                .get(self.pos + 1)
                .filter(|token| token.kind == TokenKind::Ident)
                .map_or("", |token| &self.text[token.span.range()]);
            diag = diag.help(builtin_variant_help(name, variant));
        }
        self.diags.push(diag);
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

    /// Skips to the next token that can start an item, or, in a module's
    /// body, to the `}` that closes it, so that an error in its last item
    /// does not carry the items after the module into it. `pub` is not
    /// one: the token after it is, and the item parses as well without
    /// it, so a misplaced `pub` that caused the error is not met again.
    /// Nor is a `memory {` deeper than the items: in a body it is an
    /// expression, such as the struct literal `memory { data: 1 }`; and
    /// `peripheral` followed by a name is one only at the depth of the
    /// items, as `memory {` (ADR 0058).
    fn recover_to_item(&mut self) {
        loop {
            let at_items = self.depth == self.module_depth.unwrap_or(0);
            let closes_module = self.at(TokenKind::RBrace) && self.module_depth == Some(self.depth);
            let starts_item = matches!(
                self.peek().kind,
                TokenKind::Entry
                    | TokenKind::Panic
                    | TokenKind::Fn
                    | TokenKind::Extern
                    | TokenKind::Struct
                    | TokenKind::Enum
                    | TokenKind::Const
                    | TokenKind::Static
                    | TokenKind::Comptime
                    | TokenKind::Mod
                    | TokenKind::Use
                    | TokenKind::Pound
                    | TokenKind::Eof
            ) || (at_items && (self.at_memory() || self.at_peripheral()));
            if closes_module || starts_item {
                return;
            }
            self.bump();
        }
    }

    fn item(&mut self) -> PResult<Item> {
        let mut attrs = Vec::new();
        while self.at(TokenKind::Pound) {
            attrs.push(self.attr()?);
        }
        // After the attributes, before the item's keywords (ADR 0050).
        let vis = match self.eat(TokenKind::Pub) {
            Some(token) => {
                self.misplaced_pub(token);
                Visibility::Pub
            }
            None => Visibility::Private,
        };
        if self.at_static() {
            return Ok(Item::Static(self.static_decl(attrs, vis)?));
        }
        if self.at_memory() {
            return Ok(Item::Memory(self.memory_decl(attrs, vis)?));
        }
        if self.at_peripheral() {
            return self.peripheral_item(attrs, vis);
        }
        match self.peek().kind {
            TokenKind::Entry | TokenKind::Panic | TokenKind::Fn | TokenKind::Extern => {
                Ok(Item::Fn(self.fn_decl(attrs, vis)?))
            }
            TokenKind::Struct => self.struct_item(attrs, vis),
            TokenKind::Enum => Ok(Item::Enum(self.enum_decl(attrs, vis)?)),
            TokenKind::Const => Ok(Item::Const(self.const_decl(attrs, vis)?)),
            TokenKind::Mod => Ok(Item::Mod(self.mod_decl(attrs, vis)?)),
            TokenKind::Use => Ok(Item::Use(self.use_decl(attrs, vis)?)),
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

    /// Reports `pub`, the token `pub_`, if the item after it is one that
    /// `pub` does not apply to: a `comptime` or `memory` block, which has no
    /// name, or the entry function, which nothing calls (ADR 0050, 0057).
    /// Anything else after `pub` is an item, or reported as not being one.
    fn misplaced_pub(&mut self, pub_: Token) {
        let (what, help) = match self.peek().kind {
            _ if self.at_memory() => (
                "a `memory` block",
                "a `memory` block describes the memory of the whole program and has no name for other modules to use; remove `pub` (ADR 0057)",
            ),
            TokenKind::Comptime => (
                "a `comptime` block",
                "a `comptime` block has no name for other modules to use; remove `pub` (ADR 0050)",
            ),
            TokenKind::Entry => (
                "the entry function",
                "nothing calls the entry function, so no module has a use for it; remove `pub` (ADR 0050)",
            ),
            _ => return,
        };
        self.diags.push(
            Diagnostic::error(
                "parser::misplaced-pub",
                format!("`pub` does not apply to {what}"),
            )
            .label(pub_.span, "not allowed here")
            .help(help),
        );
    }

    /// `mod name { items }` or `mod name;` (ADR 0049).
    fn mod_decl(&mut self, attrs: Vec<Attr>, vis: Visibility) -> PResult<ModDecl> {
        let start = self.bump().span;
        let name = self.ident()?;
        if let Some(semi) = self.eat(TokenKind::Semi) {
            return Ok(ModDecl {
                attrs,
                vis,
                name,
                items: None,
                span: start.to(semi.span),
            });
        }
        let Some(open) = self.eat(TokenKind::LBrace) else {
            return Err(self.error_expected("`;` or `{`"));
        };
        let saved = self.module_depth.replace(self.depth);
        let items = self.mod_items(open);
        self.module_depth = saved;
        let items = items?;
        let close = self.bump();
        Ok(ModDecl {
            attrs,
            vis,
            name,
            items: Some(items),
            span: start.to(close.span),
        })
    }

    /// The items of a module's body, after its `{`, which is `open`, up to
    /// its `}`.
    fn mod_items(&mut self, open: Token) -> PResult<Vec<Item>> {
        let mut items = Vec::new();
        loop {
            match self.peek().kind {
                TokenKind::RBrace => return Ok(items),
                TokenKind::Eof => {
                    self.diags.push(
                        Diagnostic::error("parser::unclosed-block", "unclosed block")
                            .label(open.span, "this `{` is never closed")
                            .label(self.missing_token_span(), "expected `}`"),
                    );
                    return Err(Reported);
                }
                _ => match self.item() {
                    Ok(item) => items.push(item),
                    Err(Reported) => self.recover_to_item(),
                },
            }
        }
    }

    /// `use path;`, `use path as name;` or `use prefix::{path, path as
    /// name, ...};`, with an optional trailing comma and no braces inside the
    /// braces (ADR 0049).
    fn use_decl(&mut self, attrs: Vec<Attr>, vis: Visibility) -> PResult<UseDecl> {
        let start = self.bump().span;
        let (path, braces) = self.path_until_brace(true)?;
        let (prefix, names) = if braces {
            // `prefix::{}` would bring in no name.
            if self.peek_second() == TokenKind::RBrace {
                self.bump();
                return Err(self.error_expected("identifier"));
            }
            self.bump();
            let names = self.brace_list(|p| {
                let path = p.use_inner_path()?;
                let alias = p.use_alias()?;
                Ok(UseName { path, alias })
            })?;
            (Some(path), names)
        } else {
            // `use crate;`, `use super;` or `use core;` names no item or
            // module inside the root or the parent.
            let last = path.segments[path.segments.len() - 1].name.as_str();
            if matches!(last, "crate" | "super") || (path.segments.len() == 1 && last == "core") {
                let found = self.peek();
                self.diags.push(
                    Diagnostic::error(
                        "parser::unexpected-token",
                        format!("expected `::`, found {}", found.kind.describe()),
                    )
                    .label(self.missing_token_span(), "expected `::`")
                    .help("a `use` brings in a name, and a path made only of `crate`, `super` or `core` has none: continue it with `::` and the name of an item or module, as in `use super::name;` (ADR 0049)"),
                );
                return Err(Reported);
            }
            let alias = self.use_alias()?;
            (None, vec![UseName { path, alias }])
        };
        let semi = self.expect(TokenKind::Semi)?;
        Ok(UseDecl {
            attrs,
            vis,
            prefix,
            names,
            span: start.to(semi.span),
        })
    }

    /// A path between the braces of a `use`, which continues the prefix:
    /// names only, and no braces (ADR 0049).
    fn use_inner_path(&mut self) -> PResult<Path> {
        let mut segments = vec![self.ident()?];
        while self.eat(TokenKind::ColonColon).is_some() {
            segments.push(self.ident()?);
        }
        Ok(Path {
            span: segments[0].span.to(segments[segments.len() - 1].span),
            segments,
        })
    }

    /// `as name` after a path of a `use`, if written.
    fn use_alias(&mut self) -> PResult<Option<Ident>> {
        match self.eat(TokenKind::As) {
            Some(_) => Ok(Some(self.ident()?)),
            None => Ok(None),
        }
    }

    /// `#[name]` or `#[name(N)]`.
    fn attr(&mut self) -> PResult<Attr> {
        let pound = self.bump();
        self.expect(TokenKind::LBracket)?;
        let name = self.ident()?;
        let arg = if self.eat(TokenKind::LParen).is_some() {
            let value = self.nested_expr()?;
            self.expect(TokenKind::RParen)?;
            Some(Box::new(value))
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

    fn fn_decl(&mut self, attrs: Vec<Attr>, vis: Visibility) -> PResult<FnDecl> {
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
                vis,
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
            vis,
            kind,
            name,
            params,
            ret,
            span: start.to(body.span),
            body: Some(body),
        })
    }

    fn const_decl(&mut self, attrs: Vec<Attr>, vis: Visibility) -> PResult<ConstDecl> {
        let start = self.bump().span;
        let name = self.ident()?;
        self.expect(TokenKind::Colon)?;
        let ty = self.ty()?;
        self.expect(TokenKind::Eq)?;
        let value = self.expr()?;
        let semi = self.expect(TokenKind::Semi)?;
        Ok(ConstDecl {
            attrs,
            vis,
            name,
            ty,
            value,
            span: start.to(semi.span),
        })
    }

    /// A static, or an `extern static`, which has no initializer.
    fn static_decl(&mut self, attrs: Vec<Attr>, vis: Visibility) -> PResult<StaticDecl> {
        let start = self.peek().span;
        let external = self.eat(TokenKind::Extern).is_some();
        self.expect(TokenKind::Static)?;
        let mutable = self.eat(TokenKind::Mut).is_some();
        let name = self.ident()?;
        self.expect(TokenKind::Colon)?;
        let ty = self.ty()?;
        let value = if external {
            None
        } else {
            self.expect(TokenKind::Eq)?;
            Some(self.expr()?)
        };
        let semi = self.expect(TokenKind::Semi)?;
        Ok(StaticDecl {
            attrs,
            vis,
            external,
            mutable,
            name,
            ty,
            value,
            span: start.to(semi.span),
        })
    }

    /// A struct, `struct NAME { ... }`, or a bit-field struct,
    /// `struct NAME: TYPE { ... }`, told apart by the token after the name
    /// (ADR 0059). At `struct`.
    fn struct_item(&mut self, attrs: Vec<Attr>, vis: Visibility) -> PResult<Item> {
        let start = self.bump().span;
        let name = self.ident()?;
        if self.at(TokenKind::Colon) {
            let decl = self.bit_struct_decl(attrs, vis, start, name)?;
            return Ok(Item::BitStruct(decl));
        }
        Ok(Item::Struct(self.struct_decl(attrs, vis, start, name)?))
    }

    /// The fields of a struct, `{ field: Type, ... }` (ADR 0021). `start` is
    /// the span of `struct`.
    fn struct_decl(
        &mut self,
        attrs: Vec<Attr>,
        vis: Visibility,
        start: Span,
        name: Ident,
    ) -> PResult<StructDecl> {
        self.expect(TokenKind::LBrace)?;
        let fields = self.brace_list(|p| {
            let vis = p.field_visibility();
            let name = p.ident()?;
            p.expect(TokenKind::Colon)?;
            let ty = p.ty()?;
            // Bits and defaults are a bit-field struct's (ADR 0059).
            if p.at_word("at") || p.at(TokenKind::Eq) {
                let span = p.peek().span;
                return Err(p.report_unexpected_help(
                    "`,` or `}`",
                    span,
                    "only the fields of a bit-field struct have bits, `at BITS`, and defaults, `= VALUE`; a bit-field struct names its integer type after its name, as in `struct Lcr: u8 { ... }` (ADR 0059)",
                ));
            }
            Ok(FieldDecl { vis, name, ty })
        })?;
        Ok(StructDecl {
            attrs,
            vis,
            name,
            fields,
            span: start.to(self.tokens[self.pos - 1].span),
        })
    }

    /// A field is private unless written `pub` (ADR 0050).
    fn field_visibility(&mut self) -> Visibility {
        match self.eat(TokenKind::Pub) {
            Some(_) => Visibility::Pub,
            None => Visibility::Private,
        }
    }

    /// `: Type { field, ... }` after a bit-field struct's name, with an
    /// optional trailing comma (ADR 0059). At `:`; `start` is the span of
    /// `struct`.
    fn bit_struct_decl(
        &mut self,
        attrs: Vec<Attr>,
        vis: Visibility,
        start: Span,
        name: Ident,
    ) -> PResult<BitStructDecl> {
        self.bump();
        let ty = self.ty()?;
        self.expect(TokenKind::LBrace)?;
        let mut fields = Vec::new();
        // Each field checks the token after it.
        while !self.at(TokenKind::RBrace) {
            fields.push(self.bit_field()?);
            if self.eat(TokenKind::Comma).is_none() {
                break;
            }
        }
        let close = self.expect(TokenKind::RBrace)?;
        Ok(BitStructDecl {
            attrs,
            vis,
            name,
            ty,
            fields,
            span: start.to(close.span),
        })
    }

    /// `[pub] NAME: Type at BITS [READ_EFFECT] [WRITE_EFFECT] [= default]`
    /// (ADR 0059, 0060), which `,` or `}` must follow; they are left to the
    /// caller.
    fn bit_field(&mut self) -> PResult<BitFieldDecl> {
        let start = self.peek().span;
        let vis = self.field_visibility();
        let name = self.ident()?;
        self.expect_help(TokenKind::Colon, BIT_FIELD_HELP)?;
        let ty = self.ty()?;
        self.expect_word("at", BIT_FIELD_HELP)?;
        let low = self.bit_end()?;
        let high = if self.eat(TokenKind::DotDotEq).is_some() {
            Some(self.bit_end()?)
        } else if self.at(TokenKind::DotDot) {
            // `at 0..1` would read as half-open, as a range in Rust.
            let span = self.peek().span;
            return Err(self.report_unexpected_help("`..=`", span, BITS_HELP));
        } else {
            None
        };
        let read_effect = self.read_effect(BIT_FIELD_HELP)?;
        let write_effect = self.write_effect()?;
        let default = match self.eat(TokenKind::Eq) {
            Some(_) => Some(self.nested_expr()?),
            None => None,
        };
        if !self.at(TokenKind::Comma) && !self.at(TokenKind::RBrace) {
            // What may still come, in the order of the grammar.
            let what = match (&high, &read_effect, &write_effect, &default) {
                (_, _, _, Some(_)) => "`,` or `}`",
                (_, _, Some(_), None) => "`=`, `,` or `}`",
                (_, Some(_), None, None) => "`write`, `=`, `,` or `}`",
                (Some(_), None, None, None) => "`read`, `write`, `=`, `,` or `}`",
                (None, None, None, None) => "`..=`, `read`, `write`, `=`, `,` or `}`",
            };
            return Err(self.error_expected_help(what, BIT_FIELD_HELP));
        }
        Ok(BitFieldDecl {
            vis,
            name,
            ty,
            low,
            high,
            read_effect,
            write_effect,
            default,
            span: start.to(self.tokens[self.pos - 1].span),
        })
    }

    /// An end of a bit field's bits: an expression without assignment, so
    /// that a `=` after it starts the default (ADR 0059).
    fn bit_end(&mut self) -> PResult<Expr> {
        self.expr_bp(ASSIGN_BP.0 + 1)
    }

    /// `read clears`, `read sets` or `read modifies`, if at a `read` that
    /// starts one (ADR 0060); `help` is the help of the declaration it is
    /// in.
    fn read_effect(&mut self, help: &str) -> PResult<Option<(ReadEffect, Span)>> {
        if !self.at_read_effect() {
            return Ok(None);
        }
        let read = self.bump();
        let effect = match self.word_at(self.pos) {
            Some("clears") => ReadEffect::Clears,
            Some("sets") => ReadEffect::Sets,
            Some("modifies") => ReadEffect::Modifies,
            _ => return Err(self.error_expected_help("`clears`, `sets` or `modifies`", help)),
        };
        let word = self.bump();
        Ok(Some((effect, read.span.to(word.span))))
    }

    /// `write BIT clears`, `write BIT sets` or `write BIT toggles`, if at
    /// `write`, the bit an integer literal of value 0 or 1 (ADR 0060). A
    /// `write` followed by `:` is the next field, named `write`, after a
    /// missing `,`, which is reported instead.
    fn write_effect(&mut self) -> PResult<Option<(WriteEffect, Span)>> {
        if !self.at_word("write") || self.peek_second() == TokenKind::Colon {
            return Ok(None);
        }
        let write = self.bump();
        let token = self.peek();
        let bit = match token.kind {
            TokenKind::Int => match lexer::int_value(&self.text[token.span.range()]) {
                Ok(bit @ (0 | 1)) => bit as u8,
                Ok(_) => {
                    return Err(self.report_unexpected_help(
                        "`0` or `1`",
                        token.span,
                        BIT_FIELD_HELP,
                    ));
                }
                // The lexer reported it.
                Err(_) => return Err(Reported),
            },
            _ => return Err(self.error_expected_help("`0` or `1`", BIT_FIELD_HELP)),
        };
        self.bump();
        let action = match self.word_at(self.pos) {
            Some("clears") => WriteAction::Clears,
            Some("sets") => WriteAction::Sets,
            Some("toggles") => WriteAction::Toggles,
            _ => {
                return Err(
                    self.error_expected_help("`clears`, `sets` or `toggles`", BIT_FIELD_HELP)
                );
            }
        };
        let word = self.bump();
        Ok(Some((
            WriteEffect { bit, action },
            write.span.to(word.span),
        )))
    }

    /// `enum Name: Type { Variant = value, Variant(T, ...) = value, ... }`.
    /// The integer type and every value are written, never implied
    /// (ADR 0035); a payload goes between a variant's name and its value
    /// (ADR 0039).
    fn enum_decl(&mut self, attrs: Vec<Attr>, vis: Visibility) -> PResult<EnumDecl> {
        let start = self.bump().span;
        let name = self.ident()?;
        if self.eat(TokenKind::Colon).is_none() {
            let found = self.peek();
            self.diags.push(
                Diagnostic::error(
                    "parser::enum-without-type",
                    format!("expected `:`, found {}", found.kind.describe()),
                )
                .label(self.missing_token_span(), "expected `: Type`")
                .help(format!(
                    "every enum states the integer type it is stored as, e.g. `enum {}: u8 {{ ... }}` (ADR 0035)",
                    name.name
                )),
            );
            return Err(Reported);
        }
        let ty = self.ty()?;
        self.expect(TokenKind::LBrace)?;
        let variants = self.brace_list(|p| {
            let name = p.ident()?;
            let payload = match p.eat(TokenKind::LParen) {
                Some(open) => p.payload(&name, open)?,
                None => Vec::new(),
            };
            if p.eat(TokenKind::Eq).is_none() {
                let found = p.peek();
                p.diags.push(
                    Diagnostic::error(
                        "parser::variant-without-value",
                        format!("expected `=`, found {}", found.kind.describe()),
                    )
                    .label(p.missing_token_span(), "expected `= value`")
                    .help(format!(
                        "every variant states its value, e.g. `{} = 0`, so that adding one never renumbers the others (ADR 0035)",
                        name.name
                    )),
                );
                return Err(Reported);
            }
            let value = p.nested_expr()?;
            // `Name = 0(u8)`: the payload comes before the value.
            if p.at(TokenKind::LParen) {
                let found = p.peek();
                p.diags.push(
                    Diagnostic::error(
                        "parser::unexpected-token",
                        format!("expected `,` or `}}`, found {}", found.kind.describe()),
                    )
                    .label(found.span, "expected `,` or `}`")
                    .help(format!(
                        "a payload is written after the variant's name, before its value, as in `{}(u8) = 0` (ADR 0039)",
                        name.name
                    )),
                );
                return Err(Reported);
            }
            Ok(VariantDecl {
                name,
                payload,
                value,
            })
        })?;
        Ok(EnumDecl {
            attrs,
            vis,
            name,
            ty,
            variants,
            span: start.to(self.tokens[self.pos - 1].span),
        })
    }

    /// The payload of the variant `name`, `T, ...)` after its `(`, with an
    /// optional trailing comma (ADR 0039). `()` alone is reported, and
    /// gives no types: a variant without payload has no parentheses.
    fn payload(&mut self, name: &Ident, open: Token) -> PResult<Vec<Ty>> {
        let types = self.comma_list(Self::ty)?;
        if types.is_empty() {
            let close = self.tokens[self.pos - 1].span;
            self.diags.push(
                Diagnostic::error(
                    "parser::empty-payload",
                    format!("the variant `{}` has a payload without types", name.name),
                )
                .label(open.span.to(close), "no types")
                .help(format!(
                    "a variant without payload is written without parentheses, `{}`, and one with a payload lists its types, as in `{}(u8)` (ADR 0039)",
                    name.name, name.name
                )),
            );
        }
        Ok(types)
    }

    /// `memory { ... }`, its regions and placements in any order
    /// (ADR 0057). At `memory {`.
    fn memory_decl(&mut self, attrs: Vec<Attr>, vis: Visibility) -> PResult<MemoryDecl> {
        const HELP: &str = "a `memory` block holds regions, `region NAME { origin: ADDRESS, size: BYTES }`, and placements, `code in NAME;`, `data in NAME;` and `stack in NAME { size: BYTES }` (ADR 0057)";
        let start = self.bump().span;
        self.bump();
        let mut regions = Vec::new();
        let mut placements = Vec::new();
        loop {
            if self.at_word("region") {
                regions.push(self.region_decl()?);
            } else if self.at_word("code") || self.at_word("data") || self.at_word("stack") {
                placements.push(self.placement()?);
            } else if self.at(TokenKind::RBrace) {
                break;
            } else {
                let what = "`region`, `code`, `data`, `stack` or `}`";
                if self.at(TokenKind::Eof) {
                    return Err(self.error_expected_help(what, HELP));
                }
                // A stray token, not a missing one: point at the token
                // itself.
                let span = self.peek().span;
                return Err(self.report_unexpected_help(what, span, HELP));
            }
        }
        let close = self.bump();
        Ok(MemoryDecl {
            attrs,
            vis,
            regions,
            placements,
            span: start.to(close.span),
        })
    }

    /// `region NAME { origin: expr, size: expr }`, with an optional
    /// trailing comma (ADR 0057). At `region`.
    fn region_decl(&mut self) -> PResult<RegionDecl> {
        const HELP: &str = "a region is written `region NAME { origin: ADDRESS, size: BYTES }`, with both fields, in that order (ADR 0057)";
        let start = self.bump().span;
        let name = self.ident()?;
        self.expect_help(TokenKind::LBrace, HELP)?;
        self.expect_word("origin", HELP)?;
        self.expect_help(TokenKind::Colon, HELP)?;
        let origin = self.nested_expr()?;
        // A region closed after its origin misses its size, rather than
        // the `,` before it.
        if !self.at(TokenKind::RBrace) {
            self.expect_help(TokenKind::Comma, HELP)?;
        }
        self.expect_word("size", HELP)?;
        self.expect_help(TokenKind::Colon, HELP)?;
        let size = self.nested_expr()?;
        self.eat(TokenKind::Comma);
        let close = self.expect_help(TokenKind::RBrace, HELP)?;
        Ok(RegionDecl {
            name,
            origin,
            size,
            span: start.to(close.span),
        })
    }

    /// `code in NAME;`, `data in NAME;` or `stack in NAME { size: expr }`,
    /// the stack's size with an optional trailing comma (ADR 0057). At
    /// `code`, `data` or `stack`.
    fn placement(&mut self) -> PResult<Placement> {
        const HELP: &str =
            "`code` and `data` are placed with `code in NAME;` and `data in NAME;` (ADR 0057)";
        const STACK_HELP: &str =
            "the stack is placed with its size, `stack in NAME { size: BYTES }` (ADR 0057)";
        let keyword = self.bump();
        let text = self.text;
        let word = &text[keyword.span.range()];
        let help = if word == "stack" { STACK_HELP } else { HELP };
        self.expect_word("in", help)?;
        let region = self.ident()?;
        let (kind, end) = if word == "stack" {
            self.expect_help(TokenKind::LBrace, help)?;
            self.expect_word("size", help)?;
            self.expect_help(TokenKind::Colon, help)?;
            let size = self.nested_expr()?;
            self.eat(TokenKind::Comma);
            let close = self.expect_help(TokenKind::RBrace, help)?;
            (PlacementKind::Stack { size }, close.span)
        } else {
            let semi = self.expect_help(TokenKind::Semi, help)?;
            let kind = if word == "code" {
                PlacementKind::Code
            } else {
                PlacementKind::Data
            };
            (kind, semi.span)
        };
        Ok(Placement {
            kind,
            region,
            span: keyword.span.to(end),
        })
    }

    /// `expect`, with `help` on the error.
    fn expect_help(&mut self, kind: TokenKind, help: &str) -> PResult<Token> {
        match self.eat(kind) {
            Some(token) => Ok(token),
            None => Err(self.error_expected_help(kind.describe(), help)),
        }
    }

    /// A peripheral type, `peripheral NAME { registers }`, or a peripheral,
    /// `peripheral NAME: Type at ADDRESS;`, told apart by the token after
    /// the name (ADR 0058). At `peripheral` and a name.
    fn peripheral_item(&mut self, attrs: Vec<Attr>, vis: Visibility) -> PResult<Item> {
        const HELP: &str = "`peripheral NAME { registers }` declares a peripheral type, and `peripheral NAME: TYPE at ADDRESS;` a peripheral of that type (ADR 0058)";
        let start = self.bump().span;
        let name = self.ident()?;
        if self.at(TokenKind::LBrace) {
            let decl = self.peripheral_type_decl(attrs, vis, start, name)?;
            return Ok(Item::PeripheralType(decl));
        }
        if self.at(TokenKind::Colon) {
            let decl = self.peripheral_decl(attrs, vis, start, name)?;
            return Ok(Item::Peripheral(decl));
        }
        Err(self.error_expected_help("`{` or `:`", HELP))
    }

    /// The registers of a peripheral type, `{ register, ... }`, with an
    /// optional trailing comma (ADR 0058). At `{`; `start` is the span of
    /// `peripheral`.
    fn peripheral_type_decl(
        &mut self,
        attrs: Vec<Attr>,
        vis: Visibility,
        start: Span,
        name: Ident,
    ) -> PResult<PeripheralTypeDecl> {
        self.bump();
        let mut registers = Vec::new();
        // Each register checks the token after it.
        while !self.at(TokenKind::RBrace) {
            registers.push(self.register()?);
            if self.eat(TokenKind::Comma).is_none() {
                break;
            }
        }
        let close = self.bump();
        Ok(PeripheralTypeDecl {
            attrs,
            vis,
            name,
            registers,
            span: start.to(close.span),
        })
    }

    /// `[pub] ACCESS NAME: Type at OFFSET [READ_EFFECT] [in GROUP]`
    /// (ADR 0058, 0060, 0061). The access mode is `read`, `write` or
    /// `read write`, each word one only when `:` does not follow it, since
    /// the name is the identifier just before `:`. It must be followed by
    /// `,` or `}`, which are left to the caller.
    fn register(&mut self) -> PResult<RegisterDecl> {
        let start = self.peek().span;
        // A register is private unless written `pub`, as a field (ADR 0050).
        let vis = match self.eat(TokenKind::Pub) {
            Some(_) => Visibility::Pub,
            None => Visibility::Private,
        };
        let access = if self.at_access_word("read") {
            self.bump();
            if self.at_access_word("write") {
                self.bump();
                AccessMode::ReadWrite
            } else {
                AccessMode::Read
            }
        } else if self.at_access_word("write") {
            self.bump();
            AccessMode::Write
        } else {
            let what = "`read` or `write`";
            if self.at(TokenKind::Eof) {
                return Err(self.error_expected_help(what, REGISTER_HELP));
            }
            // The mode is missing before the token, which is likely the
            // register's name: point at it rather than at the end of the
            // line before.
            let span = self.peek().span;
            return Err(self.report_unexpected_help(what, span, REGISTER_HELP));
        };
        let name = self.ident()?;
        self.expect_help(TokenKind::Colon, REGISTER_HELP)?;
        let ty = self.ty()?;
        self.expect_word("at", REGISTER_HELP)?;
        let offset = self.nested_expr()?;
        let read_effect = self.read_effect(REGISTER_END_HELP)?;
        let group = if self.at_word("in") {
            self.bump();
            Some(self.ident()?)
        } else {
            None
        };
        if !self.at(TokenKind::Comma) && !self.at(TokenKind::RBrace) {
            // What may still come, in the order of the grammar.
            let what = match (&read_effect, &group) {
                (_, Some(_)) => "`,` or `}`",
                (Some(_), None) => "`in`, `,` or `}`",
                (None, None) => "`read`, `in`, `,` or `}`",
            };
            return Err(self.error_expected_help(what, REGISTER_END_HELP));
        }
        Ok(RegisterDecl {
            vis,
            access,
            name,
            ty,
            offset,
            read_effect,
            group,
            span: start.to(self.tokens[self.pos - 1].span),
        })
    }

    /// `: Type at ADDRESS;` after a peripheral's name (ADR 0058). At `:`;
    /// `start` is the span of `peripheral`.
    fn peripheral_decl(
        &mut self,
        attrs: Vec<Attr>,
        vis: Visibility,
        start: Span,
        name: Ident,
    ) -> PResult<PeripheralDecl> {
        const HELP: &str = "a peripheral is written `peripheral NAME: TYPE at ADDRESS;`, its type a peripheral type (ADR 0058)";
        self.bump();
        let ty = self.ty()?;
        self.expect_word("at", HELP)?;
        let address = self.expr()?;
        let semi = self.expect_help(TokenKind::Semi, HELP)?;
        Ok(PeripheralDecl {
            attrs,
            vis,
            name,
            ty,
            address,
            span: start.to(semi.span),
        })
    }

    /// Consumes the identifier `word`, a word of a `memory` block or of a
    /// peripheral (ADR 0057, 0058), or reports it missing, with `help`.
    fn expect_word(&mut self, word: &str, help: &str) -> PResult<Token> {
        if self.at_word(word) {
            Ok(self.bump())
        } else {
            Err(self.error_expected_help(&format!("`{word}`"), help))
        }
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
        if self.at(TokenKind::Underscore) {
            return Err(self.underscore("identifier"));
        }
        let token = self.expect(TokenKind::Ident)?;
        Ok(Ident {
            name: self.text[token.span.range()].to_owned(),
            span: token.span,
        })
    }

    /// A path: names separated by `::`, the first of which may be `crate`
    /// or `super`, and `super` again after `super` (ADR 0049).
    fn path(&mut self) -> PResult<Path> {
        Ok(self.path_until_brace(false)?.0)
    }

    /// A path, as `path` reads it; with `brace`, a `::` followed by `{` ends
    /// it, as in a `use` with braces: the `::` is consumed, the `{` left to
    /// the caller, and the second value says so.
    fn path_until_brace(&mut self, brace: bool) -> PResult<(Path, bool)> {
        let first = match self.peek().kind {
            TokenKind::Crate | TokenKind::Super => self.keyword_segment(),
            _ => self.ident()?,
        };
        let mut supers = first.name == "super";
        let mut segments = vec![first];
        let mut braces = false;
        while self.eat(TokenKind::ColonColon).is_some() {
            if brace && self.at(TokenKind::LBrace) {
                braces = true;
                break;
            }
            let segment = if supers && self.at(TokenKind::Super) {
                self.keyword_segment()
            } else {
                supers = false;
                self.ident()?
            };
            segments.push(segment);
        }
        let path = Path {
            span: segments[0].span.to(segments[segments.len() - 1].span),
            segments,
        };
        Ok((path, braces))
    }

    /// `crate` or `super` as a segment of a path, named by its keyword.
    fn keyword_segment(&mut self) -> Ident {
        let token = self.bump();
        Ident {
            name: self.text[token.span.range()].to_owned(),
            span: token.span,
        }
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
            TokenKind::Ident | TokenKind::Crate | TokenKind::Super => {
                let path = self.path()?;
                // Type arguments, as `Option(u32)`; `Name()` parses, and is
                // a count error (ADR 0045).
                if self.eat(TokenKind::LParen).is_some() {
                    let args = self.comma_list(Self::ty)?;
                    let close = self.tokens[self.pos - 1].span;
                    return Ok(Ty {
                        kind: TyKind::Applied(path, args),
                        span: token.span.to(close),
                    });
                }
                return Ok(Ty {
                    span: path.span,
                    kind: TyKind::Named(path),
                });
            }
            TokenKind::LBracket => {
                self.bump();
                let elem = Box::new(self.ty()?);
                // `[T]`, unsized, only behind a pointer (ADR 0031).
                if let Some(close) = self.eat(TokenKind::RBracket) {
                    return Ok(Ty {
                        kind: TyKind::Slice(elem),
                        span: token.span.to(close.span),
                    });
                }
                if self.eat(TokenKind::Semi).is_none() {
                    return Err(self.error_expected("`;` or `]`"));
                }
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
                    self.const_decl(Vec::new(), Visibility::Private)?;
                }
                _ if self.at_static() => {
                    let token = self.peek();
                    self.diags.push(
                        Diagnostic::error(
                            "parser::local-static",
                            "`static` items cannot be declared inside a function",
                        )
                        .label(token.span, "inside a function")
                        .help("move the `static` out of the function (ADR 0030)"),
                    );
                    self.static_decl(Vec::new(), Visibility::Private)?;
                }
                _ if self.at_local_memory() => {
                    let token = self.peek();
                    self.diags.push(
                        Diagnostic::error(
                            "parser::local-memory",
                            "a `memory` block cannot be declared inside a function",
                        )
                        .label(token.span, "inside a function")
                        .help("move the `memory` block out of the function: it is an item, which describes the memory of the whole program (ADR 0057)"),
                    );
                    self.memory_decl(Vec::new(), Visibility::Private)?;
                }
                _ if self.at_peripheral() => {
                    let token = self.peek();
                    self.diags.push(
                        Diagnostic::error(
                            "parser::local-peripheral",
                            "`peripheral` items cannot be declared inside a function",
                        )
                        .label(token.span, "inside a function")
                        .help("move the peripheral type or peripheral out of the function: it is an item, which names a device for the whole program (ADR 0058)"),
                    );
                    self.peripheral_item(Vec::new(), Visibility::Private)?;
                }
                TokenKind::Mod => {
                    let token = self.peek();
                    self.diags.push(
                        Diagnostic::error(
                            "parser::local-mod",
                            "`mod` items cannot be declared inside a function",
                        )
                        .label(token.span, "inside a function")
                        .help("move the `mod` out of the function (ADR 0049)"),
                    );
                    self.mod_decl(Vec::new(), Visibility::Private)?;
                }
                TokenKind::Use => {
                    let token = self.peek();
                    self.diags.push(
                        Diagnostic::error(
                            "parser::local-use",
                            "`use` declarations cannot be written inside a function",
                        )
                        .label(token.span, "inside a function")
                        .help("move the `use` out of the function, to bring the name into its module (ADR 0049)"),
                    );
                    self.use_decl(Vec::new(), Visibility::Private)?;
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
            TokenKind::LBrace
                | TokenKind::If
                | TokenKind::While
                | TokenKind::Loop
                | TokenKind::Match
        )
    }

    /// Reports a `let` whose name is not followed by `: Type`; `example`
    /// shows one that is, and `adrs` the ADRs requiring it.
    fn let_without_type(&mut self, example: &str, adrs: &str) -> Reported {
        let found = self.peek();
        self.diags.push(
            Diagnostic::error(
                "parser::let-without-type",
                format!("expected `:`, found {}", found.kind.describe()),
            )
            .label(self.missing_token_span(), "expected `: Type`")
            .help(format!(
                "every `let` states its type, e.g. `{example}` ({adrs})"
            )),
        );
        Reported
    }

    /// Reports `_` where `what` is expected: it is a token of its own, the
    /// wildcard pattern, and never a name or a value (ADR 0037).
    fn underscore(&mut self, what: &str) -> Reported {
        let token = self.peek();
        self.diags.push(
            Diagnostic::error(
                "parser::unexpected-token",
                format!("expected {what}, found `_`"),
            )
            .label(token.span, format!("expected {what}"))
            .help("`_` is the wildcard pattern of `match`, and cannot be a name or a value (ADR 0037)"),
        );
        Reported
    }

    fn let_stmt(&mut self) -> PResult<Let> {
        let start = self.bump().span;
        let mutable = self.eat(TokenKind::Mut).is_some();
        let name = self.ident()?;
        if !self.at(TokenKind::Colon) {
            let example = format!("let {}: u32 = ...", name.name);
            return Err(self.let_without_type(&example, "ADR 0008"));
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
            let op_span = self.bump().span;
            let rhs = self.expr_bp(right_bp)?;
            let span = lhs.span.to(rhs.span);
            let (l, r) = (Box::new(lhs), Box::new(rhs));
            let kind = match op {
                Infix::Binary(op) => ExprKind::Binary(op, l, r, op_span),
                Infix::Logical(op) => ExprKind::Logical(op, l, r),
                Infix::Assign => ExprKind::Assign(l, r),
                Infix::CompoundAssign(op) => ExprKind::CompoundAssign(op, l, r, op_span),
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

    /// A primary expression followed by any indexing, field accesses and
    /// `?` (ADR 0046), which all bind as tightly.
    fn postfix(&mut self) -> PResult<Expr> {
        let mut expr = self.primary()?;
        loop {
            if let Some(open) = self.eat(TokenKind::LBracket) {
                let index = self.nested_expr()?;
                let close = self.expect(TokenKind::RBracket)?;
                expr = Expr {
                    span: expr.span.to(close.span),
                    kind: ExprKind::Index(Box::new(expr), Box::new(index), open.span),
                };
            } else if self.eat(TokenKind::Dot).is_some() {
                let field = self.ident()?;
                expr = Expr {
                    span: expr.span.to(field.span),
                    kind: ExprKind::Field(Box::new(expr), field),
                };
            } else if let Some(question) = self.eat(TokenKind::Question) {
                expr = Expr {
                    span: expr.span.to(question.span),
                    kind: ExprKind::Try(Box::new(expr), question.span),
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
            TokenKind::ByteString => {
                self.bump();
                ExprKind::ByteString(lexer::byte_string_value(text).ok())
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
            TokenKind::Match => return self.match_expr(),
            TokenKind::Underscore => return Err(self.underscore("an expression")),
            TokenKind::While => {
                self.bump();
                let cond = Box::new(self.condition()?);
                let body = self.block()?;
                return Ok(Expr {
                    span: token.span.to(body.span),
                    kind: ExprKind::While { cond, body },
                });
            }
            TokenKind::Ident | TokenKind::Crate | TokenKind::Super => {
                let path = self.path()?;
                // Arguments as a call's, whatever the path names
                // (ADR 0039).
                if self.eat(TokenKind::LParen).is_some() {
                    let args = self.comma_list(Self::nested_expr)?;
                    let end = self.tokens[self.pos - 1].span;
                    // `Option(u32)::None` is no path: an error at the `::`
                    // shows the variant as `.None` (ADR 0045).
                    if let Some(name) = path.as_single()
                        && is_builtin_enum(&name.name)
                    {
                        self.after_builtin_call = Some((self.pos, name.name.clone()));
                    }
                    return Ok(Expr {
                        span: token.span.to(end),
                        kind: ExprKind::Call(path, args),
                    });
                }
                // Whatever the path names: `uart::Config { .. }` is a
                // struct of a module (ADR 0049).
                if !self.no_struct && self.eat(TokenKind::LBrace).is_some() {
                    let (fields, rest) = self.struct_lit_fields()?;
                    let end = self.tokens[self.pos - 1].span;
                    return Ok(Expr {
                        span: token.span.to(end),
                        kind: ExprKind::StructLit(path, fields, rest),
                    });
                }
                return Ok(Expr {
                    span: path.span,
                    kind: ExprKind::Name(path),
                });
            }
            // A variant named by the expected type, its arguments parsed
            // as a call's (ADR 0044). After an expression, `.` is a field
            // access instead, in `postfix`.
            TokenKind::Dot => {
                self.bump();
                let variant = self.ident()?;
                let args = match self.eat(TokenKind::LParen) {
                    Some(_) => Some(self.comma_list(Self::nested_expr)?),
                    None => None,
                };
                let end = self.tokens[self.pos - 1].span;
                return Ok(Expr {
                    span: token.span.to(end),
                    kind: ExprKind::ContextVariant(variant, args),
                });
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
    /// The fields of a struct literal, `field: value, ...}` after its `{`,
    /// with an optional trailing comma or a final `..` (ADR 0059), whose
    /// span is the second value; consumes the `}`.
    fn struct_lit_fields(&mut self) -> PResult<(Vec<FieldInit>, Option<Span>)> {
        let mut fields = Vec::new();
        let mut rest = None;
        while !self.at(TokenKind::RBrace) {
            if let Some(dots) = self.eat(TokenKind::DotDot) {
                rest = Some(dots.span);
                if !self.at(TokenKind::RBrace) {
                    return Err(self.error_expected_help("`}`", REST_HELP));
                }
                break;
            }
            let name = self.ident()?;
            self.expect(TokenKind::Colon)?;
            let value = self.nested_expr()?;
            fields.push(FieldInit { name, value });
            if self.eat(TokenKind::Comma).is_none() {
                if self.at(TokenKind::DotDot) {
                    return Err(self.error_expected_help("`,` or `}`", REST_HELP));
                }
                break;
            }
        }
        self.expect(TokenKind::RBrace)?;
        Ok((fields, rest))
    }

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

    /// The length of an array type or repeat expression, computed at
    /// compile time (ADR 0029).
    fn array_len(&mut self) -> PResult<Box<Expr>> {
        Ok(Box::new(self.nested_expr()?))
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

impl Parser<'_> {
    /// `match scrutinee { arms }` (ADR 0037). As in the condition of `if`,
    /// a `{` after the scrutinee opens the arms.
    fn match_expr(&mut self) -> PResult<Expr> {
        let start = self.bump().span;
        let scrutinee = Box::new(self.condition()?);
        self.expect(TokenKind::LBrace)?;
        // The arms are delimited, so struct literals are allowed again.
        let saved = std::mem::replace(&mut self.no_struct, false);
        let arms = self.arms();
        self.no_struct = saved;
        let arms = arms?;
        let close = self.tokens[self.pos - 1].span;
        Ok(Expr {
            kind: ExprKind::Match {
                scrutinee,
                arms,
                keyword: start,
            },
            span: start.to(close),
        })
    }

    /// `pattern => expr, ...}`, allowing a trailing comma, and consumes the
    /// `}`. As a statement does, an arm whose expression starts with a
    /// block-like expression ends with it and needs no comma: in `0 => {}`
    /// followed by `-1 => {}`, the `-1` starts the next arm.
    fn arms(&mut self) -> PResult<Vec<Arm>> {
        let mut arms = Vec::new();
        while !self.at(TokenKind::RBrace) {
            let pattern = self.pattern()?;
            self.expect(TokenKind::FatArrow)?;
            let block_like = self.at_block_like();
            let body = if block_like {
                self.primary()?
            } else {
                self.expr()?
            };
            arms.push(Arm { pattern, body });
            if self.eat(TokenKind::Comma).is_none() && !block_like {
                if self.at(TokenKind::RBrace) {
                    break;
                }
                return Err(self.error_expected("`,` or `}`"));
            }
        }
        self.expect(TokenKind::RBrace)?;
        Ok(arms)
    }

    /// A pattern, or alternatives separated by `|`.
    fn pattern(&mut self) -> PResult<Pattern> {
        let first = self.single_pattern()?;
        if !self.at(TokenKind::Pipe) {
            return Ok(first);
        }
        let mut alternatives = vec![first];
        while self.eat(TokenKind::Pipe).is_some() {
            alternatives.push(self.single_pattern()?);
        }
        let span = alternatives[0]
            .span
            .to(alternatives[alternatives.len() - 1].span);
        Ok(Pattern {
            kind: PatternKind::Or(alternatives),
            span,
        })
    }

    /// One pattern of ADR 0037: `_`, `let [mut] name: Type`, a literal, `-`
    /// and an integer literal, a name, `Enum::Variant`, or
    /// `Enum::Variant(p, ...)`, each `p` a pattern with alternatives, with
    /// an optional trailing comma (ADR 0040); or `.Variant` and
    /// `.Variant(p, ...)` likewise (ADR 0044).
    fn single_pattern(&mut self) -> PResult<Pattern> {
        let token = self.peek();
        let text = &self.text[token.span.range()];
        let kind = match token.kind {
            TokenKind::Underscore => PatternKind::Wildcard,
            TokenKind::Int => PatternKind::Int {
                value: lexer::int_value(text).ok(),
                negated: false,
            },
            TokenKind::Byte => PatternKind::Byte(lexer::byte_value(text).ok()),
            TokenKind::True | TokenKind::False => PatternKind::Bool(token.kind == TokenKind::True),
            TokenKind::Ident | TokenKind::Crate | TokenKind::Super => {
                let path = self.path()?;
                // `Enum::Variant()` parses, and is a count error
                // (ADR 0040); a bare name takes no patterns.
                if path.as_single().is_none() && self.eat(TokenKind::LParen).is_some() {
                    let patterns = self.comma_list(Self::pattern)?;
                    let end = self.tokens[self.pos - 1].span;
                    return Ok(Pattern {
                        span: path.span.to(end),
                        kind: PatternKind::PathArgs(path, patterns),
                    });
                }
                return Ok(Pattern {
                    span: path.span,
                    kind: PatternKind::Name(path),
                });
            }
            TokenKind::Minus => {
                self.bump();
                let literal = self.peek();
                if literal.kind != TokenKind::Int {
                    return Err(self.error_expected("an integer literal"));
                }
                self.bump();
                return Ok(Pattern {
                    kind: PatternKind::Int {
                        value: lexer::int_value(&self.text[literal.span.range()]).ok(),
                        negated: true,
                    },
                    span: token.span.to(literal.span),
                });
            }
            // A variant of the enum that is the type of its position
            // (ADR 0044); `.Variant()` parses, and is a count error.
            TokenKind::Dot => {
                self.bump();
                let variant = self.ident()?;
                let patterns = match self.eat(TokenKind::LParen) {
                    Some(_) => Some(self.comma_list(Self::pattern)?),
                    None => None,
                };
                let end = self.tokens[self.pos - 1].span;
                return Ok(Pattern {
                    span: token.span.to(end),
                    kind: PatternKind::ContextVariant(variant, patterns),
                });
            }
            TokenKind::Let => return self.binding_pattern(),
            _ => return Err(self.error_expected("a pattern")),
        };
        self.bump();
        Ok(Pattern {
            kind,
            span: token.span,
        })
    }

    /// `let [mut] name: Type`, a new variable, always with its type
    /// (ADR 0008, 0037).
    fn binding_pattern(&mut self) -> PResult<Pattern> {
        let start = self.bump().span;
        let mutable = self.eat(TokenKind::Mut).is_some();
        let name = self.ident()?;
        if !self.at(TokenKind::Colon) {
            let example = format!("let {}: u8 => ...", name.name);
            return Err(self.let_without_type(&example, "ADR 0008, 0037"));
        }
        self.bump();
        let ty = self.ty()?;
        Ok(Pattern {
            span: start.to(ty.span),
            kind: PatternKind::Binding { mutable, name, ty },
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::span::SourceMap;

    /// After an error in the last item of a module, parsing resumes at the
    /// module's `}`, which closes it: the items after it are not in it
    /// (ADR 0049).
    #[test]
    fn items_after_a_broken_module_are_outside_it() {
        let text = "mod broken { fn first() {} fn last() { let } } struct After {}";
        let mut sources = SourceMap::default();
        let file = sources.add("main.ers", text);
        let mut diags = Diagnostics::default();
        let tokens = lexer::lex(file, text, &mut diags);
        let ast = parse(text, &tokens, &mut diags);
        let codes: Vec<&str> = diags.into_vec().iter().map(|d| d.code).collect();
        assert!(!codes.contains(&"parser::unclosed-block"), "{codes:?}");
        let [Item::Mod(module), Item::Struct(after)] = ast.items.as_slice() else {
            panic!("a module, then a struct: {:#?}", ast.items);
        };
        let inside = module.items.as_deref().unwrap_or_default();
        assert!(
            matches!(inside, [Item::Fn(first), ..] if first.name.name == "first")
                && !inside.iter().any(|item| matches!(item, Item::Struct(_))),
            "{inside:#?}"
        );
        assert_eq!(after.name.name, "After");
    }
}
