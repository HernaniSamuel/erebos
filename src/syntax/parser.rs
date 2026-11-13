//! Recursive-descent parser (ADR 0004). Errors are reported and the parser
//! resynchronizes at the next item, so one run reports several errors.

use crate::builtin::{builtin_variant_help, is_builtin_enum};
use crate::diag::{Diagnostic, Diagnostics};
use crate::span::Span;
use crate::syntax::ast::{
    AccessMode, Arm, AsmExpr, AsmOperand, AsmOperandKind, AsmOption, AsmOptionKind, AsmReg, Attr,
    BinOp, BitFieldDecl, BitStructDecl, Block, ClusterDecl, ComptimeDecl, ConstDecl, EntryDecl,
    EnumDecl, Expr, ExprKind, FieldDecl, FieldInit, File, FnDecl, FnKind, Ident, ImplDecl, Item,
    Let, LogicalOp, MemoryDecl, ModDecl, Param, Path, Pattern, PatternKind, PeripheralDecl,
    PeripheralTypeDecl, Placement, PlacementKind, ReadEffect, Receiver, ReceiverKind, RegionDecl,
    RegisterDecl, StaticDecl, Stmt, StrLit, StructDecl, SymTarget, Ty, TyKind, UnOp, UseDecl,
    UseName, VariantDecl, Visibility, WriteAction, WriteEffect,
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
const REGISTER_HELP: &str = "a register is written `ACCESS NAME: TYPE at OFFSET`, optionally after `pub`, its access mode `read`, `write` or `read write`, in that order, and its name the identifier just before `:`; an entry with no access mode, `NAME: TYPE at OFFSET`, is a cluster; entries are separated by `,` (ADR 0058, 0064)";

/// The help of the errors after a register's offset (ADR 0060, 0061).
const REGISTER_END_HELP: &str = "after its offset, a register may declare the distance between the elements of a register array, `step STEP`, what a read does, `read clears`, `read sets` or `read modifies`, then an alternate group, `in GROUP`, in that order; entries are separated by `,` (ADR 0060, 0061, 0064)";

/// The help of the errors in a cluster (ADR 0064).
const CLUSTER_HELP: &str = "a cluster is written `NAME: TYPE at OFFSET`, optionally after `pub`, then optionally `step STEP` for an array of them and an alternate group, `in GROUP`, in that order; entries are separated by `,` (ADR 0064)";

/// The help of the errors in a field of a bit-field struct (ADR 0059,
/// 0060).
const BIT_FIELD_HELP: &str = "a bit field is written `NAME: TYPE at BITS`, optionally after `pub` and before the name `read` or `write`, then optionally a read effect, `read clears`, `read sets` or `read modifies`, a write effect, `write 0` or `write 1` followed by `clears`, `sets` or `toggles`, and a default, `= VALUE`, in that order; fields are separated by `,` (ADR 0059, 0060, 0065)";

/// The help of `..=` in an index (ADR 0108).
const CLOSED_RANGE_HELP: &str = "a range in an index is half-open: `base[a..b]` is the elements from `a` up to but not including `b`; write `base[a..b + 1]` to include `b` (ADR 0108)";

/// The help of `..` alone in an index (ADR 0108).
const WHOLE_RANGE_HELP: &str = "a range in an index needs a bound: write `&*s` for the whole of a slice `s`, or let a reference to an array convert to a `&[T]` (ADR 0108)";

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
        if found.kind == TokenKind::String {
            return self.string_outside_asm(found.span);
        }
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
        if found.kind == TokenKind::String {
            return self.string_outside_asm(found.span);
        }
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

    /// Reports the string literal at `span`, which only an `asm` expression
    /// takes (ADR 0079); a byte string is the text of the rest of the
    /// language (ADR 0016, 0031). The caller skips it, or recovers.
    fn string_outside_asm(&mut self, span: Span) -> Reported {
        self.diags.push(
            Diagnostic::error(
                "parser::string-outside-asm",
                "string literals exist only inside `asm`",
            )
            .label(span, "not valid here")
            .help("text is bytes outside `asm`: write a byte string, `b\"...\"`, with the same escapes (ADR 0016, 0031, 0079)"),
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

    /// Skips to the next token that can start an item, or, in a module's
    /// body, to the `}` that closes it, so that an error in its last item
    /// does not carry the items after the module into it. `pub` is not
    /// one: the token after it is, and the item parses as well without
    /// it, so a misplaced `pub` that caused the error is not met again.
    /// Nor is a `memory {` deeper than the items: in a body it is an
    /// expression, such as the struct literal `memory { data: 1 }`; and
    /// `peripheral` followed by a name is one only at the depth of the
    /// items, as `memory {` (ADR 0058). `depth` counts only braces, so at
    /// that depth either is one only where an item may start (see
    /// `at_item_start`): not in a constant's initializer, after an
    /// operator on the same line.
    fn recover_to_item(&mut self) {
        loop {
            let at_items = self.depth == self.module_depth.unwrap_or(0) && self.at_item_start();
            let closes_module = self.at(TokenKind::RBrace) && self.module_depth == Some(self.depth);
            let starts_item = matches!(
                self.peek().kind,
                TokenKind::Entry
                    | TokenKind::Panic
                    | TokenKind::Fn
                    | TokenKind::Extern
                    | TokenKind::Impl
                    | TokenKind::Struct
                    | TokenKind::Enum
                    | TokenKind::Const
                    | TokenKind::Static
                    | TokenKind::Comptime
                    | TokenKind::Mod
                    | TokenKind::Use
                    | TokenKind::Pound
                    | TokenKind::Eof
            ) || (at_items && (self.at_memory() || self.at_peripheral()))
                // A keyword that cannot be a name, so `interrupt` alone is
                // the misplaced name of something (ADR 0084).
                || (self.at(TokenKind::Interrupt) && self.peek_second() == TokenKind::Fn);
            if closes_module || starts_item {
                return;
            }
            self.bump();
        }
    }

    /// Whether the next token is where an item may start: the first of the
    /// file, the first on its line, or after `;`, `}` or `pub`.
    fn at_item_start(&self) -> bool {
        let Some(prev) = self.pos.checked_sub(1).map(|i| self.tokens[i]) else {
            return true;
        };
        let gap = &self.text[prev.span.hi as usize..self.peek().span.lo as usize];
        gap.contains('\n')
            || matches!(
                prev.kind,
                TokenKind::Semi | TokenKind::RBrace | TokenKind::Pub
            )
    }

    fn item(&mut self) -> PResult<Item> {
        let (attrs, vis) = self.attrs_and_vis()?;
        self.item_after(attrs, vis)
    }

    /// The attributes and the `pub` before an item or a function of a type.
    fn attrs_and_vis(&mut self) -> PResult<(Vec<Attr>, Visibility)> {
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
        Ok((attrs, vis))
    }

    /// The item after its attributes and `pub`.
    fn item_after(&mut self, attrs: Vec<Attr>, vis: Visibility) -> PResult<Item> {
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
            TokenKind::Entry
            | TokenKind::Panic
            | TokenKind::Interrupt
            | TokenKind::Fn
            | TokenKind::Extern => Ok(Item::Fn(self.fn_decl(attrs, vis, false)?)),
            TokenKind::Impl => Ok(Item::Impl(self.impl_decl(attrs, vis)?)),
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
            TokenKind::Impl => (
                "an `impl` block",
                "an `impl` block has no name for other modules to use; `pub` goes on each function in it, to make it visible wherever the type is (ADR 0113)",
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

    /// `impl Name { fns }` (ADR 0113): functions with a body, each with the
    /// attributes and the `pub` it takes; anything else between the braces
    /// is reported and parsed to the end of the item.
    fn impl_decl(&mut self, attrs: Vec<Attr>, vis: Visibility) -> PResult<ImplDecl> {
        let start = self.bump().span;
        let name = self.ident()?;
        let Some(open) = self.eat(TokenKind::LBrace) else {
            return Err(self.error_expected("`{`"));
        };
        // A `}` met while recovering from an error in a function closes the
        // `impl`, as it closes a module.
        let saved = self.module_depth.replace(self.depth);
        let fns = self.impl_items(open);
        self.module_depth = saved;
        let fns = fns?;
        let close = self.bump();
        Ok(ImplDecl {
            attrs,
            vis,
            name,
            fns,
            span: start.to(close.span),
        })
    }

    /// The functions of an `impl`, after its `{`, which is `open`, up to its
    /// `}`.
    fn impl_items(&mut self, open: Token) -> PResult<Vec<FnDecl>> {
        let mut fns = Vec::new();
        loop {
            if self.at(TokenKind::RBrace) {
                return Ok(fns);
            }
            if self.at(TokenKind::Eof) {
                self.diags.push(
                    Diagnostic::error("parser::unclosed-block", "unclosed block")
                        .label(open.span, "this `{` is never closed")
                        .label(self.missing_token_span(), "expected `}`"),
                );
                return Err(Reported);
            }
            match self.impl_item() {
                Ok(Some(decl)) => fns.push(decl),
                Ok(None) => {}
                Err(Reported) => self.recover_to_item(),
            }
        }
    }

    /// One item between the braces of an `impl`: a function with a body, or
    /// `None` for what is reported as not being one (ADR 0113).
    fn impl_item(&mut self) -> PResult<Option<FnDecl>> {
        let (attrs, vis) = self.attrs_and_vis()?;
        if self.at(TokenKind::Fn) {
            return Ok(Some(self.fn_decl(attrs, vis, true)?));
        }
        let token = self.peek();
        let what = match token.kind {
            _ if self.at_static() => "a `static`",
            _ if self.at_memory() => "a `memory` block",
            _ if self.at_peripheral() => "a peripheral item",
            TokenKind::Extern => "an `extern fn`",
            TokenKind::Entry => "the entry function",
            TokenKind::Panic => "the panic function",
            TokenKind::Interrupt => "an interrupt handler",
            TokenKind::Struct => "a struct",
            TokenKind::Enum => "an enum",
            TokenKind::Const => "a constant",
            TokenKind::Mod => "a module",
            TokenKind::Use => "a `use`",
            TokenKind::Comptime => "a `comptime` block",
            TokenKind::Impl => "an `impl` block",
            _ => return Err(self.error_expected("`fn` or `}`")),
        };
        self.diags.push(
            Diagnostic::error(
                "parser::impl-item",
                format!("{what} cannot be declared inside an `impl`"),
            )
            .label(token.span, "inside an `impl`")
            .help("an `impl` block holds only functions with a body, `fn` and `pub fn`; declare other items outside it, in the module (ADR 0113)"),
        );
        // Consumed, so that the rest of the block parses.
        self.item_after(attrs, vis)?;
        Ok(None)
    }

    fn fn_decl(&mut self, attrs: Vec<Attr>, vis: Visibility, in_impl: bool) -> PResult<FnDecl> {
        let start = self.peek().span;
        let external = self.eat(TokenKind::Extern).is_some();
        let mut interrupt = false;
        let kind = if external {
            FnKind::Plain
        } else if self.eat(TokenKind::Entry).is_some() {
            FnKind::Entry
        } else if self.eat(TokenKind::Panic).is_some() {
            FnKind::Panic
        } else if self.at(TokenKind::Interrupt) {
            // Its vector follows the signature; the kind is set then.
            self.bump();
            interrupt = true;
            FnKind::Plain
        } else {
            FnKind::Plain
        };
        self.expect(TokenKind::Fn)?;
        let name = self.ident()?;
        self.expect(TokenKind::LParen)?;
        let (receiver, params) = self.params(in_impl)?;
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
                receiver,
                params,
                ret,
                span: start.to(semi.span),
                body: None,
            });
        }
        // `at` follows the signature of an `interrupt fn`, where it is
        // required, and of nothing else (ADR 0084).
        let kind = if interrupt {
            if !self.at_word("at") {
                return Err(self.error_expected_help(
                    "`at`",
                    "an interrupt handler names the vector it serves after its signature, as in `interrupt fn tick() at Vector::SysTick { ... }` (ADR 0084)",
                ));
            }
            self.bump();
            FnKind::Interrupt(self.path()?)
        } else if self.at_word("at") {
            return Err(self.error_expected_help(
                "`{`",
                "only an `interrupt fn` names a vector with `at`: declare it `interrupt fn name() at Vector::Name { ... }` (ADR 0084)",
            ));
        } else {
            kind
        };
        let body = self.block()?;
        Ok(FnDecl {
            attrs,
            vis,
            kind,
            name,
            receiver,
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
        let access = if self.at_access_word("read") || self.at_access_word("write") {
            let mode = if self.at_word("read") {
                AccessMode::Read
            } else {
                AccessMode::Write
            };
            let word = self.bump();
            // `read write` is what no word says: the field is not both.
            if mode == AccessMode::Read && self.at_access_word("write") {
                let span = self.peek().span;
                return Err(self.report_unexpected_help(
                    "the field's name",
                    span,
                    "a field is `read` or `write`, and has neither word when it is both; the name is the identifier just before `:` (ADR 0065)",
                ));
            }
            Some((mode, word.span))
        } else {
            None
        };
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
            access,
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
    /// `write` followed by `:` is the next field, named `write`, and one
    /// followed by a name and `:` a `write` field (ADR 0065), after a
    /// missing `,`, which is reported instead.
    fn write_effect(&mut self) -> PResult<Option<(WriteEffect, Span)>> {
        let third = self
            .tokens
            .get(self.pos + 2)
            .map_or(TokenKind::Eof, |token| token.kind);
        if !self.at_word("write")
            || self.peek_second() == TokenKind::Colon
            || (self.peek_second() == TokenKind::Ident && third == TokenKind::Colon)
        {
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

    /// The entries of a peripheral type, `{ entry, ... }`, with an optional
    /// trailing comma (ADR 0058, 0064). At `{`; `start` is the span of
    /// `peripheral`.
    fn peripheral_type_decl(
        &mut self,
        attrs: Vec<Attr>,
        vis: Visibility,
        start: Span,
        name: Ident,
    ) -> PResult<PeripheralTypeDecl> {
        self.bump();
        let mut entries = Vec::new();
        // Each entry checks the token after it.
        while !self.at(TokenKind::RBrace) {
            entries.push(self.entry()?);
            if self.eat(TokenKind::Comma).is_none() {
                break;
            }
        }
        let close = self.bump();
        Ok(PeripheralTypeDecl {
            attrs,
            vis,
            name,
            entries,
            span: start.to(close.span),
        })
    }

    /// A register or a cluster (ADR 0064): after an optional `pub`, an
    /// entry whose first word is followed by `:` is a cluster of that name,
    /// so that `read: T` is a cluster named `read`; any other starts with an
    /// access mode, a register. It must be followed by `,` or `}`, which are
    /// left to the caller.
    fn entry(&mut self) -> PResult<EntryDecl> {
        let start = self.peek().span;
        // An entry is private unless written `pub`, as a field (ADR 0050).
        let vis = match self.eat(TokenKind::Pub) {
            Some(_) => Visibility::Pub,
            None => Visibility::Private,
        };
        if self.at(TokenKind::Ident) && self.peek_second() == TokenKind::Colon {
            return self.cluster(start, vis).map(EntryDecl::Cluster);
        }
        self.register(start, vis).map(EntryDecl::Register)
    }

    /// `NAME: Type at OFFSET [step STEP] [in GROUP]` (ADR 0064), after an
    /// optional `pub`, at the name; `start` is the span of the entry's first
    /// token.
    fn cluster(&mut self, start: Span, vis: Visibility) -> PResult<ClusterDecl> {
        let name = self.ident()?;
        self.bump();
        let ty = self.ty()?;
        self.expect_word("at", CLUSTER_HELP)?;
        let offset = self.nested_expr()?;
        let step = self.step()?;
        let group = self.group()?;
        if !self.at(TokenKind::Comma) && !self.at(TokenKind::RBrace) {
            // What may still come, in the order of the grammar.
            let what = match (&step, &group) {
                (_, Some(_)) => "`,` or `}`",
                (Some(_), None) => "`in`, `,` or `}`",
                (None, None) => "`step`, `in`, `,` or `}`",
            };
            return Err(self.error_expected_help(what, CLUSTER_HELP));
        }
        Ok(ClusterDecl {
            vis,
            name,
            ty,
            offset,
            step,
            group,
            span: start.to(self.tokens[self.pos - 1].span),
        })
    }

    /// `step STEP` after an entry's offset, if at the word `step` (ADR 0064).
    /// A `step` followed by `:` is the next entry, a cluster named `step`,
    /// after a missing `,`, which is reported instead.
    fn step(&mut self) -> PResult<Option<Expr>> {
        if !self.at_word("step") || self.peek_second() == TokenKind::Colon {
            return Ok(None);
        }
        self.bump();
        Ok(Some(self.nested_expr()?))
    }

    /// `in GROUP` after an entry, if at the word `in` (ADR 0061).
    fn group(&mut self) -> PResult<Option<Ident>> {
        // An `in` followed by `:` is the next entry, a cluster named `in`,
        // after a missing `,`, which is reported instead.
        if !self.at_word("in") || self.peek_second() == TokenKind::Colon {
            return Ok(None);
        }
        self.bump();
        Ok(Some(self.ident()?))
    }

    /// `ACCESS NAME: Type at OFFSET [step STEP] [READ_EFFECT] [in GROUP]`
    /// (ADR 0058, 0060, 0061, 0064), after an optional `pub`; `start` is the
    /// span of the entry's first token. The access mode is `read`, `write`
    /// or `read write`, each word one only when `:` does not follow it,
    /// since the name is the identifier just before `:`.
    fn register(&mut self, start: Span, vis: Visibility) -> PResult<RegisterDecl> {
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
            // The token is no mode: point at it rather than at the end of
            // the line before. A name before `:` is a cluster (`entry`).
            let found = self.peek();
            return Err(self.report_unexpected_help(what, found.span, REGISTER_HELP));
        };
        let name = self.ident()?;
        self.expect_help(TokenKind::Colon, REGISTER_HELP)?;
        let ty = self.ty()?;
        self.expect_word("at", REGISTER_HELP)?;
        let offset = self.nested_expr()?;
        let step = self.step()?;
        let read_effect = self.read_effect(REGISTER_END_HELP)?;
        let group = self.group()?;
        if !self.at(TokenKind::Comma) && !self.at(TokenKind::RBrace) {
            // What may still come, in the order of the grammar.
            let what = match (&step, &read_effect, &group) {
                (_, _, Some(_)) => "`,` or `}`",
                (_, Some(_), None) => "`in`, `,` or `}`",
                (Some(_), None, None) => "`read`, `in`, `,` or `}`",
                (None, None, None) => "`step`, `read`, `in`, `,` or `}`",
            };
            return Err(self.error_expected_help(what, REGISTER_END_HELP));
        }
        Ok(RegisterDecl {
            vis,
            access,
            name,
            ty,
            offset,
            step,
            read_effect,
            group,
            span: start.to(self.tokens[self.pos - 1].span),
        })
    }

    /// `: Type at ADDRESS [over PATH];` after a peripheral's name
    /// (ADR 0058, 0066). At `:`;
    /// `start` is the span of `peripheral`.
    fn peripheral_decl(
        &mut self,
        attrs: Vec<Attr>,
        vis: Visibility,
        start: Span,
        name: Ident,
    ) -> PResult<PeripheralDecl> {
        const HELP: &str = "a peripheral is written `peripheral NAME: TYPE at ADDRESS;`, its type a peripheral type, and optionally `over PERIPHERAL` before the `;` when it is another view of the device of that peripheral (ADR 0058, 0066)";
        self.bump();
        let ty = self.ty()?;
        self.expect_word("at", HELP)?;
        let address = self.expr()?;
        let over = if self.at_word("over") {
            self.bump();
            Some(self.path()?)
        } else {
            None
        };
        let semi = match self.eat(TokenKind::Semi) {
            Some(semi) => semi,
            None => {
                let what = if over.is_some() {
                    "`;`"
                } else {
                    "`over` or `;`"
                };
                return Err(self.error_expected_help(what, HELP));
            }
        };
        Ok(PeripheralDecl {
            attrs,
            vis,
            name,
            ty,
            address,
            over,
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

    /// The parameters after the `(` of a function: in an `impl`, the first
    /// may be a receiver (ADR 0113). Consumes the `)`.
    fn params(&mut self, in_impl: bool) -> PResult<(Option<Receiver>, Vec<Param>)> {
        let mut receiver = None;
        if in_impl && self.at_receiver() {
            receiver = Some(self.receiver());
            if self.eat(TokenKind::Comma).is_none() {
                self.expect(TokenKind::RParen)?;
                return Ok((receiver, Vec::new()));
            }
        }
        let params = self.comma_list(Self::param)?;
        Ok((receiver, params))
    }

    /// At `self`, `mut self`, `&self` or `&mut self`: the forms of a
    /// receiver (ADR 0113).
    fn at_receiver(&self) -> bool {
        let at = |offset: usize, kind: TokenKind| {
            self.tokens
                .get(self.pos + offset)
                .is_some_and(|token| token.kind == kind)
        };
        match self.peek().kind {
            TokenKind::SelfValue => true,
            TokenKind::Mut => at(1, TokenKind::SelfValue),
            TokenKind::Amp => {
                at(1, TokenKind::SelfValue)
                    || (at(1, TokenKind::Mut) && at(2, TokenKind::SelfValue))
            }
            _ => false,
        }
    }

    /// The receiver at `at_receiver`.
    fn receiver(&mut self) -> Receiver {
        let start = self.peek().span;
        let kind = if self.eat(TokenKind::Amp).is_some() {
            if self.eat(TokenKind::Mut).is_some() {
                ReceiverKind::RefMut
            } else {
                ReceiverKind::Ref
            }
        } else if self.eat(TokenKind::Mut).is_some() {
            ReceiverKind::MutValue
        } else {
            ReceiverKind::Value
        };
        let word = self.bump();
        Receiver {
            kind,
            span: start.to(word.span),
        }
    }

    fn param(&mut self) -> PResult<Param> {
        // A receiver is the first parameter of a function of a type, and
        // nothing else (ADR 0113): reported, and parsed as the parameter it
        // would be named for, so that the rest of the signature parses.
        if self.at_receiver() {
            let receiver = self.receiver();
            self.diags.push(
                Diagnostic::error(
                    "parser::misplaced-self",
                    format!(
                        "`{}` can only be the first parameter of a function in an `impl`",
                        receiver.kind.written()
                    ),
                )
                .label(receiver.span, "not allowed here")
                .help("a receiver is the first parameter of a function in an `impl` block, as in `fn count(&self) -> usize`; it names the value the function is called on (ADR 0113)"),
            );
            return Ok(Param {
                mutable: false,
                name: Ident {
                    name: "self".to_owned(),
                    span: receiver.span,
                },
                ty: Ty {
                    kind: TyKind::Unit,
                    span: receiver.span,
                },
            });
        }
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
            TokenKind::Crate | TokenKind::Super | TokenKind::SelfValue | TokenKind::SelfType => {
                self.keyword_segment()
            }
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

    /// `crate`, `super`, `self` or `Self` as a segment of a path, named by
    /// its keyword.
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
            TokenKind::Ident | TokenKind::Crate | TokenKind::Super | TokenKind::SelfType => {
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
            TokenKind::Amp => {
                self.bump();
                return self.ref_ty(token.span);
            }
            TokenKind::AndAnd => {
                // `&&T` is `& &T`, rejected later (ADR 0105).
                self.bump();
                let (outer, inner) = split_double_amp(token.span);
                let referent = self.ref_ty(inner)?;
                return Ok(Ty {
                    span: outer.to(referent.span),
                    kind: TyKind::Ref {
                        mutable: false,
                        volatile: false,
                        referent: Box::new(referent),
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

    /// The rest of `&T`, `&mut T`, `&volatile T` or `&mut volatile T` after
    /// its `&`, which spans `amp` (ADR 0110).
    fn ref_ty(&mut self, amp: Span) -> PResult<Ty> {
        let mutable = self.eat(TokenKind::Mut).is_some();
        let volatile = self.eat(TokenKind::Volatile).is_some();
        let referent = self.ty()?;
        Ok(Ty {
            span: amp.to(referent.span),
            kind: TyKind::Ref {
                mutable,
                volatile,
                referent: Box::new(referent),
            },
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
                TokenKind::Impl => {
                    let token = self.peek();
                    self.diags.push(
                        Diagnostic::error(
                            "parser::local-impl",
                            "`impl` blocks cannot be declared inside a function",
                        )
                        .label(token.span, "inside a function")
                        .help("move the `impl` out of the function, to the module of its type (ADR 0113)"),
                    );
                    self.impl_decl(Vec::new(), Visibility::Private)?;
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
                | TokenKind::Critical
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
            TokenKind::Amp => {
                // `&raw const` and `&raw mut` are addresses (ADR 0021);
                // any other `&` borrows (ADR 0105).
                if self.word_at(self.pos + 1) == Some("raw")
                    && matches!(
                        self.tokens.get(self.pos + 2).map(|token| token.kind),
                        Some(TokenKind::Const | TokenKind::Mut)
                    )
                {
                    return self.addr_of();
                }
                return self.borrow(token.span);
            }
            TokenKind::AndAnd => {
                // `&&e` is `& &e`, rejected later (ADR 0105).
                let (outer, inner) = split_double_amp(token.span);
                self.bump();
                let borrowed = self.borrow_after(inner)?;
                return Ok(Expr {
                    span: outer.to(borrowed.span),
                    kind: ExprKind::Ref {
                        mutable: false,
                        expr: Box::new(borrowed),
                    },
                });
            }
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

    /// `&e` or `&mut e`, at the `&`, which spans `amp` (ADR 0105).
    fn borrow(&mut self, amp: Span) -> PResult<Expr> {
        self.bump();
        self.borrow_after(amp)
    }

    /// The rest of `&e` or `&mut e` after its `&`, which spans `amp`.
    fn borrow_after(&mut self, amp: Span) -> PResult<Expr> {
        let mutable = self.eat(TokenKind::Mut).is_some();
        let expr = Box::new(self.expr_bp(PREFIX_BP)?);
        Ok(Expr {
            span: amp.to(expr.span),
            kind: ExprKind::Ref { mutable, expr },
        })
    }

    /// `&raw const place` or `&raw mut place`, at the `&`, which `prefix`
    /// has checked is followed by `raw` and `const` or `mut` (ADR 0021).
    fn addr_of(&mut self) -> PResult<Expr> {
        let amp = self.bump();
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

    /// A primary expression followed by any indexing, field accesses, method
    /// calls (ADR 0114) and `?` (ADR 0046), which all bind as tightly.
    fn postfix(&mut self) -> PResult<Expr> {
        let mut expr = self.primary()?;
        loop {
            if let Some(open) = self.eat(TokenKind::LBracket) {
                expr = self.index_or_range(expr, open.span)?;
            } else if self.eat(TokenKind::Dot).is_some() {
                let field = self.ident()?;
                // `.name(` is a method call (ADR 0114), `.name` a field.
                if self.eat(TokenKind::LParen).is_some() {
                    let args = self.comma_list(Self::nested_expr)?;
                    let end = self.tokens[self.pos - 1].span;
                    expr = Expr {
                        span: expr.span.to(end),
                        kind: ExprKind::MethodCall(Box::new(expr), field, args),
                    };
                } else {
                    expr = Expr {
                        span: expr.span.to(field.span),
                        kind: ExprKind::Field(Box::new(expr), field),
                    };
                }
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

    /// The rest of `base[index]`, `base[a..b]`, `base[a..]` or `base[..b]`
    /// after its `[`, which spans `open`. `..` binds more loosely than every
    /// operator and `as`, and exists only here (ADR 0108); `..` alone and
    /// `..=` are errors, with a help for each.
    fn index_or_range(&mut self, base: Expr, open: Span) -> PResult<Expr> {
        let from = if self.at(TokenKind::DotDot) || self.at(TokenKind::DotDotEq) {
            None
        } else {
            let index = self.nested_expr()?;
            if !self.at(TokenKind::DotDot) && !self.at(TokenKind::DotDotEq) {
                let close = self.expect(TokenKind::RBracket)?;
                return Ok(Expr {
                    span: base.span.to(close.span),
                    kind: ExprKind::Index(Box::new(base), Box::new(index), open),
                });
            }
            Some(Box::new(index))
        };
        let dots = self.peek();
        if dots.kind == TokenKind::DotDotEq {
            return Err(self.report_unexpected_help("`..`", dots.span, CLOSED_RANGE_HELP));
        }
        self.bump();
        let to = if self.at(TokenKind::RBracket) {
            if from.is_none() {
                return Err(self.report_unexpected_help(
                    "a bound next to `..`",
                    dots.span,
                    WHOLE_RANGE_HELP,
                ));
            }
            None
        } else {
            Some(Box::new(self.nested_expr()?))
        };
        let close = self.expect(TokenKind::RBracket)?;
        Ok(Expr {
            span: base.span.to(close.span),
            kind: ExprKind::Subslice {
                base: Box::new(base),
                from,
                to,
                open,
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
            TokenKind::ByteString => {
                self.bump();
                ExprKind::ByteString(lexer::byte_string_value(text).ok())
            }
            // Skipped, standing for a value, so that the rest of the
            // statement parses and its own errors are found too.
            TokenKind::String => {
                self.string_outside_asm(token.span);
                self.bump();
                ExprKind::ByteString(None)
            }
            TokenKind::Asm => return Ok(self.asm_expr()),
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
            TokenKind::Ident
            | TokenKind::Crate
            | TokenKind::Super
            | TokenKind::SelfValue
            | TokenKind::SelfType => {
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
            // `critical BLOCK`, a block expression like `loop` (ADR 0086).
            TokenKind::Critical => {
                self.bump();
                let body = self.block()?;
                return Ok(Expr {
                    span: token.span.to(body.span),
                    kind: ExprKind::Critical(body),
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
    /// no value. The `,` ends it where one separates items: the arms of a
    /// `match`, the arguments of a call, the elements of an array, the
    /// fields of a struct literal.
    fn at_expr_end(&self) -> bool {
        matches!(
            self.peek().kind,
            TokenKind::Semi
                | TokenKind::Comma
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
            TokenKind::Ident | TokenKind::Crate | TokenKind::Super | TokenKind::SelfType => {
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

/// The help of the errors in an `asm` expression (ADR 0079).
const ASM_HELP: &str = "an `asm` expression is written `asm(\"template\", operands, options(...))`: one or more template strings, then the operands without a name, the named operands and the operands with an explicit register (and `clobber_abi`), in that order, then `options(...)`, which comes last (ADR 0079)";

/// How an operand of `asm` is placed in the template, and so in the order of
/// the operands (ADR 0079): the groups come in this order.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum AsmGroup {
    /// Without a name: `{}` and `{N}` take it.
    Positional,
    /// With a name: `{name}` takes it.
    Named,
    /// With an explicit register, and `clobber_abi`: no template takes it.
    Explicit,
}

impl AsmGroup {
    fn describe(self) -> &'static str {
        match self {
            AsmGroup::Positional => "an operand without a name",
            AsmGroup::Named => "a named operand",
            AsmGroup::Explicit => "an operand with an explicit register",
        }
    }
}

impl Parser<'_> {
    /// `asm(...)` (ADR 0079). The words of its operands (`in`, `out`,
    /// `lateout`, `inout`, `sym`, `clobber_abi`, `options`, `reg` and the
    /// options) are recognized here by their text, and are names anywhere
    /// else. After an error the expression ends at its closing `)`, so that
    /// the statement around it parses on.
    fn asm_expr(&mut self) -> Expr {
        let keyword = self.bump();
        let open = self.pos;
        let mut asm = AsmExpr {
            templates: Vec::new(),
            operands: Vec::new(),
            options: Vec::new(),
        };
        let end = match self.asm_args(&mut asm) {
            Ok(close) => close.span,
            Err(Reported) => self.recover_asm(open),
        };
        Expr {
            span: keyword.span.to(end),
            kind: ExprKind::Asm(asm),
        }
    }

    /// The parenthesized arguments of `asm`, up to the `)`, which it returns.
    fn asm_args(&mut self, asm: &mut AsmExpr) -> PResult<Token> {
        self.asm_expect(TokenKind::LParen)?;
        asm.templates.push(self.asm_string("a template string")?);
        let mut group = AsmGroup::Positional;
        while self.eat(TokenKind::Comma).is_some() && !self.at(TokenKind::RParen) {
            if self.at(TokenKind::String) && asm.operands.is_empty() {
                asm.templates.push(self.asm_string("a template string")?);
            } else if self.at_word("options") && self.peek_second() == TokenKind::LParen {
                self.asm_options(asm)?;
                self.eat(TokenKind::Comma);
                if !self.at(TokenKind::RParen) {
                    let span = self.missing_token_span();
                    let found = self.peek().kind.describe();
                    self.diags.push(
                        Diagnostic::error(
                            "parser::unexpected-token",
                            format!("expected `)`, found {found}"),
                        )
                        .label(span, "expected `)`")
                        .help("`options(...)` is the last argument of `asm`; the operands come before it (ADR 0079)"),
                    );
                    return Err(Reported);
                }
                break;
            } else {
                let operand = self.asm_operand()?;
                group = self.asm_order(group, &operand);
                asm.operands.push(operand);
            }
        }
        if !self.at(TokenKind::RParen) {
            return Err(self.asm_expected("`,` or `)`"));
        }
        Ok(self.bump())
    }

    /// Reports an operand that comes in the wrong group, or a name on an
    /// operand that no template places; returns the group the operands so
    /// far reached, `group` or the operand's.
    fn asm_order(&mut self, group: AsmGroup, operand: &AsmOperand) -> AsmGroup {
        let own = match (&operand.kind, &operand.name) {
            (AsmOperandKind::ClobberAbi(_), _)
            | (
                AsmOperandKind::In {
                    reg: AsmReg::Explicit(_),
                    ..
                }
                | AsmOperandKind::Out {
                    reg: AsmReg::Explicit(_),
                    ..
                }
                | AsmOperandKind::InOut {
                    reg: AsmReg::Explicit(_),
                    ..
                },
                _,
            ) => AsmGroup::Explicit,
            (_, Some(_)) => AsmGroup::Named,
            (_, None) => AsmGroup::Positional,
        };
        if let Some(name) = &operand.name
            && own == AsmGroup::Explicit
        {
            self.diags.push(
                Diagnostic::error(
                    "parser::unexpected-token",
                    "an operand with an explicit register, or `clobber_abi`, has no name",
                )
                .label(name.span, "a template cannot place this operand")
                .help("only the operands whose register the compiler chooses, `sym` and `const` take a name, and `{name}` places them; an explicit register is part of the operand (ADR 0079)"),
            );
        } else if own < group {
            // Where its group starts, which is the operand's first token.
            let at = operand.name.as_ref().map_or(operand.span, |name| name.span);
            self.diags.push(
                Diagnostic::error(
                    "parser::unexpected-token",
                    format!("{} cannot come after {}", own.describe(), group.describe()),
                )
                .label(at, "comes too late")
                .help("operands come in three groups, in this order: without a name, named, and with an explicit register (with `clobber_abi`) (ADR 0079)"),
            );
        }
        group.max(own)
    }

    /// `[name =] direction(where) ...`, `sym path`, `const expr` or
    /// `clobber_abi("C")`.
    fn asm_operand(&mut self) -> PResult<AsmOperand> {
        let start = self.peek().span;
        let name = if self.at(TokenKind::Ident) && self.peek_second() == TokenKind::Eq {
            let name = self.ident()?;
            self.bump();
            Some(name)
        } else {
            None
        };
        let word = self.word_at(self.pos).map(str::to_owned);
        let kind = match word.as_deref() {
            Some("in") => {
                self.bump();
                let reg = self.asm_where()?;
                let expr = self.nested_expr()?;
                AsmOperandKind::In { reg, expr }
            }
            Some(direction @ ("out" | "lateout")) => {
                let late = direction == "lateout";
                self.bump();
                let reg = self.asm_where()?;
                let place = self.asm_out_place()?;
                AsmOperandKind::Out { reg, late, place }
            }
            Some("inout") => {
                self.bump();
                let reg = self.asm_where()?;
                let expr = self.nested_expr()?;
                let out = match self.eat(TokenKind::FatArrow) {
                    Some(_) => Some(self.asm_out_place()?),
                    None => None,
                };
                AsmOperandKind::InOut { reg, expr, out }
            }
            Some("sym") => {
                self.bump();
                // `entry` is a keyword, so no path means it and nothing
                // follows it (ADR 0083).
                match self.eat(TokenKind::Entry) {
                    Some(token) => AsmOperandKind::Sym(SymTarget::Entry(token.span)),
                    None => AsmOperandKind::Sym(SymTarget::Path(self.path()?)),
                }
            }
            Some("clobber_abi") => {
                self.bump();
                self.asm_expect(TokenKind::LParen)?;
                let abi = self.asm_string("the name of an ABI")?;
                self.asm_expect(TokenKind::RParen)?;
                AsmOperandKind::ClobberAbi(abi)
            }
            _ if self.at(TokenKind::Const) => {
                self.bump();
                AsmOperandKind::Const(self.nested_expr()?)
            }
            _ => {
                return Err(self.asm_expected(
                    "an operand: `in`, `out`, `lateout`, `inout`, `sym`, `const` or `clobber_abi`",
                ));
            }
        };
        Ok(AsmOperand {
            name,
            kind,
            span: start.to(self.tokens[self.pos - 1].span),
        })
    }

    /// `(reg)` or `("name")`, after a direction.
    fn asm_where(&mut self) -> PResult<AsmReg> {
        self.asm_expect(TokenKind::LParen)?;
        let reg = if self.at_word("reg") {
            AsmReg::Class(self.ident()?)
        } else if self.at(TokenKind::String) {
            AsmReg::Explicit(self.asm_string("a register")?)
        } else {
            return Err(self.asm_expected("`reg` or the name of a register as a string"));
        };
        self.asm_expect(TokenKind::RParen)?;
        Ok(reg)
    }

    /// The place an output stores into, or `_`, which drops the value.
    fn asm_out_place(&mut self) -> PResult<Option<Expr>> {
        if self.eat(TokenKind::Underscore).is_some() {
            Ok(None)
        } else {
            Ok(Some(self.nested_expr()?))
        }
    }

    /// `options(option, ...)`, at `options`.
    fn asm_options(&mut self, asm: &mut AsmExpr) -> PResult<()> {
        self.bump();
        self.asm_expect(TokenKind::LParen)?;
        loop {
            let word = self.word_at(self.pos).map(str::to_owned);
            let kind = AsmOptionKind::ALL
                .iter()
                .find(|(name, _)| Some(*name) == word.as_deref())
                .map(|&(_, kind)| kind);
            let Some(kind) = kind else {
                return Err(self.asm_expected(
                    "an option: `pure`, `nomem`, `readonly`, `nostack`, `preserves_flags` or `noreturn`",
                ));
            };
            let token = self.bump();
            asm.options.push(AsmOption {
                kind,
                span: token.span,
            });
            if self.eat(TokenKind::Comma).is_none() || self.at(TokenKind::RParen) {
                break;
            }
        }
        self.asm_expect(TokenKind::RParen)?;
        Ok(())
    }

    /// A string literal, the template or the name of a register or of an
    /// ABI (`what`), with the bytes it holds.
    fn asm_string(&mut self, what: &str) -> PResult<StrLit> {
        if !self.at(TokenKind::String) {
            return Err(self.asm_expected(what));
        }
        let token = self.bump();
        // The lexer reported a malformed literal; its bytes are not needed.
        let (bytes, starts) =
            lexer::string_bytes(&self.text[token.span.range()]).unwrap_or_default();
        Ok(StrLit {
            bytes,
            span: token.span,
            starts: starts.into_iter().map(|start| start as u32).collect(),
        })
    }

    fn asm_expect(&mut self, kind: TokenKind) -> PResult<Token> {
        match self.eat(kind) {
            Some(token) => Ok(token),
            None => Err(self.asm_expected(kind.describe())),
        }
    }

    /// Reports that `what` is missing in the arguments of `asm`. Unlike
    /// `report_unexpected`, a string literal found here is reported as the
    /// unexpected token it is: it is the one place where strings are valid.
    fn asm_expected(&mut self, what: &str) -> Reported {
        let found = self.peek().kind.describe();
        let span = self.missing_token_span();
        self.diags.push(
            Diagnostic::error(
                "parser::unexpected-token",
                format!("expected {what}, found {found}"),
            )
            .label(span, format!("expected {what}"))
            .help(ASM_HELP),
        );
        Reported
    }

    /// Skips the rest of an `asm` expression after an error, to the `)` that
    /// closes it, which it consumes; or, not to swallow the code after it, to
    /// a `;` or `}` outside braces opened since the `asm`, even inside its
    /// parentheses, or to the end of the file. `open`
    /// is the position of its `(`. Returns the span of the last token
    /// consumed.
    fn recover_asm(&mut self, open: usize) -> Span {
        let (mut parens, mut braces) = (0isize, 0isize);
        for token in &self.tokens[open..self.pos] {
            match token.kind {
                TokenKind::LParen | TokenKind::LBracket => parens += 1,
                TokenKind::RParen | TokenKind::RBracket => parens -= 1,
                TokenKind::LBrace => braces += 1,
                TokenKind::RBrace => braces -= 1,
                _ => {}
            }
        }
        loop {
            let mut closes = false;
            match self.peek().kind {
                TokenKind::Eof => break,
                TokenKind::Semi | TokenKind::RBrace if braces <= 0 => break,
                TokenKind::LParen | TokenKind::LBracket => parens += 1,
                TokenKind::RParen | TokenKind::RBracket => {
                    parens -= 1;
                    closes = parens <= 0;
                }
                TokenKind::LBrace => braces += 1,
                TokenKind::RBrace => braces -= 1,
                _ => {}
            }
            self.bump();
            if closes {
                break;
            }
        }
        self.tokens[self.pos.saturating_sub(1)].span
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

/// The spans of the two `&` of an `&&` token.
fn split_double_amp(span: Span) -> (Span, Span) {
    let mid = span.lo + 1;
    (Span { hi: mid, ..span }, Span { lo: mid, ..span })
}

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
