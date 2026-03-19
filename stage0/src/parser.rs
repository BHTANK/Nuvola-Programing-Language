/// Nuvola recursive-descent parser — Stage 0
///
/// Consumes the token stream produced by `lexer::tokenize()` (which already
/// includes INDENT / DEDENT tokens) and emits an AST (`ast::Program`).
///
/// Expression precedence (low → high, matching `nuvola.peg`):
///
///   or / ||
///   and / &&
///   not / !
///   == != < > <= >= is
///   .. ..=                   (range — non-associative)
///   + -
///   * / // %                 (mul calls pipe, so pipe binds tighter)
///   |>                       (pipe)
///   **                       (power — right-associative)
///   unary -  !  not
///   postfix  .  []  ()  ?.
///   primary

use crate::ast::*;
use crate::error::ParseError;
use crate::token::{Keyword as Kw, Op, Token, TokenKind};

type PResult<T> = Result<T, ParseError>;

// ─────────────────────────────────────────────────────────────────────────────
// Public entry point
// ─────────────────────────────────────────────────────────────────────────────

pub fn parse(tokens: Vec<Token>) -> PResult<Program> {
    Parser::new(tokens).parse_program()
}

pub fn parse_spanned(tokens: Vec<Token>) -> PResult<Vec<(u32, Stmt)>> {
    Parser::new(tokens).parse_program_spanned()
}

// ─────────────────────────────────────────────────────────────────────────────
// Parser struct
// ─────────────────────────────────────────────────────────────────────────────

struct Parser {
    tokens: Vec<Token>,
    pos:    usize,
}

impl Parser {
    fn new(tokens: Vec<Token>) -> Self {
        Parser { tokens, pos: 0 }
    }

    // ── Navigation ───────────────────────────────────────────────────────────

    fn peek(&self) -> &TokenKind {
        &self.tokens[self.pos].kind
    }

    fn peek_nth(&self, n: usize) -> &TokenKind {
        let idx = (self.pos + n).min(self.tokens.len().saturating_sub(1));
        &self.tokens[idx].kind
    }

    fn peek_tok(&self) -> &Token {
        &self.tokens[self.pos]
    }

    fn advance(&mut self) -> Token {
        let tok = self.tokens[self.pos].clone();
        if self.pos + 1 < self.tokens.len() {
            self.pos += 1;
        }
        tok
    }

    fn at_eof(&self) -> bool {
        matches!(self.peek(), TokenKind::Eof)
    }

    // ── Error construction ───────────────────────────────────────────────────

    fn err(&self, msg: impl Into<String>) -> ParseError {
        let t = self.peek_tok();
        ParseError::new(msg, t.span.line, t.span.col)
    }

    fn err_expected(&self, what: &str) -> ParseError {
        self.err(format!("expected {}, found {}", what, self.peek()))
    }

    // ── Token-kind predicates ────────────────────────────────────────────────

    fn at_kw(&self, kw: Kw) -> bool {
        self.peek() == &TokenKind::Kw(kw)
    }

    fn at_op(&self, op: Op) -> bool {
        self.peek() == &TokenKind::Op(op)
    }

    fn at_newline(&self) -> bool {
        matches!(self.peek(), TokenKind::Newline)
    }

    fn at_indent(&self) -> bool {
        matches!(self.peek(), TokenKind::Indent)
    }

    fn at_dedent(&self) -> bool {
        matches!(self.peek(), TokenKind::Dedent)
    }

    fn at_ident(&self) -> bool {
        matches!(self.peek(), TokenKind::Ident(_))
    }

    // ── Eating helpers ───────────────────────────────────────────────────────

    fn skip_newlines(&mut self) {
        while matches!(self.peek(), TokenKind::Newline) {
            self.advance();
        }
    }

    /// Skip newlines AND indent/dedent tokens — used inside bracket-delimited
    /// collection literals where indentation is not semantically significant.
    /// Returns the net number of Indent tokens consumed minus Dedent tokens
    /// consumed, so callers can drain the matching Dedents at the closing bracket.
    fn skip_ws(&mut self) {
        while matches!(
            self.peek(),
            TokenKind::Newline | TokenKind::Indent | TokenKind::Dedent
        ) {
            self.advance();
        }
    }

    /// Eat any Dedent tokens that are pending (up to `n`).
    /// Used after closing brackets to drain Dedents injected inside the bracket.
    fn drain_dedents(&mut self, n: usize) {
        let mut remaining = n;
        while remaining > 0 {
            self.skip_newlines();
            if self.at_dedent() { self.advance(); remaining -= 1; } else { break; }
        }
    }

    /// Count net indentation depth change while skipping whitespace inside brackets.
    /// Returns net_indent = indents_seen - dedents_seen.
    fn skip_ws_counted(&mut self) -> i32 {
        let mut net = 0i32;
        while matches!(
            self.peek(),
            TokenKind::Newline | TokenKind::Indent | TokenKind::Dedent
        ) {
            match self.peek() {
                TokenKind::Indent => net += 1,
                TokenKind::Dedent => net -= 1,
                _ => {}
            }
            self.advance();
        }
        net
    }

    fn eat_kw(&mut self, kw: Kw) -> PResult<Token> {
        if self.at_kw(kw.clone()) {
            Ok(self.advance())
        } else {
            Err(self.err(format!("expected `{}`", kw)))
        }
    }

    fn eat_op(&mut self, op: Op) -> PResult<()> {
        if self.at_op(op.clone()) {
            self.advance();
            Ok(())
        } else {
            Err(self.err(format!("expected `{}`", op)))
        }
    }

    fn eat_indent(&mut self) -> PResult<()> {
        if self.at_indent() {
            self.advance();
            Ok(())
        } else {
            Err(self.err("expected indented block"))
        }
    }

    fn eat_dedent(&mut self) -> PResult<()> {
        if self.at_dedent() {
            self.advance();
            Ok(())
        } else {
            // Lenient: if we're at EOF or a Dedent is missing (can happen in
            // partial sources during development), don't hard-fail.
            if self.at_eof() { return Ok(()); }
            Err(self.err("expected dedent (mismatched indentation)"))
        }
    }

    fn eat_ident(&mut self) -> PResult<String> {
        match self.peek().clone() {
            TokenKind::Ident(s) => { self.advance(); Ok(s) }
            // Allow `self` as an identifier in some positions (method params).
            TokenKind::Kw(Kw::Self_) => { self.advance(); Ok("self".into()) }
            _ => Err(self.err_expected("identifier")),
        }
    }

    fn eat_str_lit(&mut self) -> PResult<String> {
        match self.peek().clone() {
            TokenKind::Str(s) => { self.advance(); Ok(s) }
            _ => Err(self.err_expected("string literal")),
        }
    }

    // ── Block parsing ─────────────────────────────────────────────────────────

    /// Parse `Indent stmts Dedent`.
    fn parse_block(&mut self) -> PResult<Vec<Stmt>> {
        self.skip_newlines();
        self.eat_indent()?;
        let stmts = self.parse_stmt_seq()?;
        self.eat_dedent()?;
        Ok(stmts)
    }

    /// Parse a sequence of statements until Dedent or Eof.
    fn parse_stmt_seq(&mut self) -> PResult<Vec<Stmt>> {
        let mut stmts = Vec::new();
        loop {
            self.skip_newlines();
            if self.at_dedent() || self.at_eof() {
                break;
            }
            stmts.push(self.parse_stmt()?);
            // `;` separates same-line statements: `a = 0; b = 1`
            while self.at_op(Op::Semi) {
                self.advance();
                self.skip_newlines();
                if self.at_dedent() || self.at_eof() { break; }
                // A trailing `;` with nothing after is fine — skip
                if !self.at_op(Op::Semi) && !self.at_newline() {
                    stmts.push(self.parse_stmt()?);
                }
            }
            // Consume at most one trailing newline; extra ones are eaten at the
            // top of the loop.
            if self.at_newline() {
                self.advance();
            }
        }
        Ok(stmts)
    }

    /// Parse the body of an if/for/while statement: either `=> stmt` or a block.
    fn parse_stmt_body(&mut self) -> PResult<Vec<Stmt>> {
        self.skip_newlines();
        if self.at_op(Op::FatArrow) {
            self.advance(); // eat =>
            let s = self.parse_stmt()?;
            Ok(vec![s])
        } else if self.at_indent() {
            self.parse_block()
        } else {
            Err(self.err("expected `=>` or indented block"))
        }
    }

    /// Parse a function body: either `=> expr` or an indented block.
    fn parse_fn_body(&mut self) -> PResult<FnBody> {
        self.skip_newlines();
        if self.at_op(Op::FatArrow) {
            self.advance();
            let e = self.parse_expr()?;
            Ok(FnBody::Arrow(Box::new(e)))
        } else if self.at_indent() {
            Ok(FnBody::Block(self.parse_block()?))
        } else {
            Err(self.err("expected `=>` or indented block after function signature"))
        }
    }

    // ═════════════════════════════════════════════════════════════════════════
    // Statements
    // ═════════════════════════════════════════════════════════════════════════

    pub fn parse_program(&mut self) -> PResult<Program> {
        let mut stmts = Vec::new();
        self.skip_newlines();
        while !self.at_eof() {
            stmts.push(self.parse_stmt()?);
            self.skip_newlines();
        }
        Ok(stmts)
    }

    /// Like parse_program but returns (source_line, Stmt) pairs for #line emission.
    pub fn parse_program_spanned(&mut self) -> PResult<Vec<(u32, Stmt)>> {
        let mut stmts = Vec::new();
        self.skip_newlines();
        while !self.at_eof() {
            let line = self.peek_tok().span.line;
            stmts.push((line, self.parse_stmt()?));
            self.skip_newlines();
        }
        Ok(stmts)
    }

    fn parse_stmt(&mut self) -> PResult<Stmt> {
        match self.peek().clone() {
            // ── Annotations ──────────────────────────────────────────────────
            TokenKind::Annot(_) => self.parse_annotation(),

            // ── Import ───────────────────────────────────────────────────────
            TokenKind::Kw(Kw::Import) => self.parse_import(),

            // ── Type declarations ─────────────────────────────────────────────
            TokenKind::Kw(Kw::Type) => self.parse_type_decl(),

            // ── Trait ────────────────────────────────────────────────────────
            TokenKind::Kw(Kw::Trait) => self.parse_trait_decl(),

            // ── Impl ─────────────────────────────────────────────────────────
            TokenKind::Kw(Kw::Impl) => self.parse_impl_decl(),

            // ── Comptime ─────────────────────────────────────────────────────
            TokenKind::Kw(Kw::Comptime) => self.parse_comptime(),

            // ── Async fn ─────────────────────────────────────────────────────
            TokenKind::Kw(Kw::Async) => {
                self.advance(); // eat `async`
                self.eat_kw(Kw::Fn)?;
                let def = self.parse_fn_def(true)?;
                Ok(Stmt::AsyncFnDecl(def))
            }

            // ── Extern fn ────────────────────────────────────────────────────
            TokenKind::Kw(Kw::Extern) => self.parse_extern_fn(),

            // ── Unsafe ───────────────────────────────────────────────────────
            TokenKind::Kw(Kw::Unsafe) => {
                self.advance();
                let body = self.parse_block()?;
                Ok(Stmt::Unsafe(body))
            }

            // ── fn ───────────────────────────────────────────────────────────
            TokenKind::Kw(Kw::Fn) => {
                self.advance();
                let def = self.parse_fn_def(false)?;
                Ok(Stmt::FnDecl(def))
            }

            // ── if ───────────────────────────────────────────────────────────
            TokenKind::Kw(Kw::If) => self.parse_if_stmt(),

            // ── for ──────────────────────────────────────────────────────────
            TokenKind::Kw(Kw::For) => self.parse_for(),

            // ── while ────────────────────────────────────────────────────────
            TokenKind::Kw(Kw::While) => self.parse_while(),

            // ── match ────────────────────────────────────────────────────────
            TokenKind::Kw(Kw::Match) => self.parse_match_stmt(),

            // ── return ───────────────────────────────────────────────────────
            TokenKind::Kw(Kw::Return) => {
                self.advance();
                let e = if self.at_newline() || self.at_dedent() || self.at_eof() {
                    None
                } else {
                    Some(Box::new(self.parse_expr()?))
                };
                Ok(Stmt::Return(e))
            }

            // ── break ────────────────────────────────────────────────────────
            TokenKind::Kw(Kw::Break) => {
                self.advance();
                let e = if self.at_newline() || self.at_dedent() || self.at_eof() {
                    None
                } else {
                    Some(Box::new(self.parse_expr()?))
                };
                Ok(Stmt::Break(e))
            }

            // ── continue ─────────────────────────────────────────────────────
            TokenKind::Kw(Kw::Continue) => {
                self.advance();
                Ok(Stmt::Continue)
            }

            // ── await (stmt form) ─────────────────────────────────────────────
            TokenKind::Kw(Kw::Await) => {
                self.advance();
                Ok(Stmt::AwaitStmt(Box::new(self.parse_expr()?)))
            }

            // ── spawn (stmt form) ─────────────────────────────────────────────
            TokenKind::Kw(Kw::Spawn) => {
                self.advance();
                Ok(Stmt::SpawnStmt(Box::new(self.parse_expr()?)))
            }

            // ── throw expr ───────────────────────────────────────────────────
            TokenKind::Kw(Kw::Throw) => {
                self.advance();
                Ok(Stmt::Throw(Box::new(self.parse_expr()?)))
            }

            // ── try / catch ──────────────────────────────────────────────────
            TokenKind::Kw(Kw::Try) => {
                self.advance();
                self.skip_newlines();
                let body = self.parse_block()?;
                let mut catches = Vec::new();
                self.skip_newlines();
                while matches!(self.peek(), TokenKind::Kw(Kw::Catch)) {
                    self.advance(); // eat `catch`
                    let var = match self.peek().clone() {
                        TokenKind::Ident(s) => { self.advance(); s }
                        _ => "_e".to_string(),
                    };
                    self.skip_newlines();
                    let handler = self.parse_block()?;
                    catches.push((var, handler));
                    self.skip_newlines();
                }
                Ok(Stmt::TryCatch { body, catches })
            }

            // ── Tuple destructure `(a, b) := expr` ───────────────────────────
            TokenKind::Op(Op::LParen) if self.looks_like_destructure() => {
                self.parse_tuple_destructure()
            }

            // ── Everything else: typed let, plain let, assign, or expr stmt ──
            _ => self.parse_assign_or_expr_stmt(),
        }
    }

    // ── @annotation ──────────────────────────────────────────────────────────

    fn parse_annotation(&mut self) -> PResult<Stmt> {
        let name = match self.advance().kind {
            TokenKind::Annot(s) => s,
            _ => unreachable!(),
        };
        self.skip_newlines();
        let inner = self.parse_stmt()?;
        Ok(Stmt::Annotation { name, inner: Box::new(inner) })
    }

    // ── import ───────────────────────────────────────────────────────────────

    fn parse_import(&mut self) -> PResult<Stmt> {
        self.advance(); // eat `import`
        // import "path.nvl" [as alias] — file-based import (string literal)
        if let TokenKind::Str(s) = self.peek().clone() {
            let s = s.clone();
            self.advance();
            // Optional `as alias`
            let alias = if self.at_kw(Kw::As) {
                self.advance();
                Some(self.eat_ident()?)
            } else {
                None
            };
            return Ok(Stmt::Import { path: vec![s], names: None, alias });
        }
        // Parse module path: a.b.c
        let mut path = vec![self.eat_ident()?];
        while self.at_op(Op::Dot) && matches!(self.peek_nth(1), TokenKind::Ident(_)) {
            self.advance(); // eat '.'
            path.push(self.eat_ident()?);
        }
        // Optional `.{name, name, ...}`
        let names = if self.at_op(Op::Dot) && self.peek_nth(1) == &TokenKind::Op(Op::LBrace) {
            self.advance(); // eat '.'
            self.advance(); // eat '{'
            let mut ns = vec![self.eat_ident()?];
            while self.at_op(Op::Comma) {
                self.advance();
                if self.at_op(Op::RBrace) { break; }
                ns.push(self.eat_ident()?);
            }
            self.eat_op(Op::RBrace)?;
            Some(ns)
        } else {
            None
        };
        // Optional `as alias`
        let alias = if self.at_kw(Kw::As) {
            self.advance();
            Some(self.eat_ident()?)
        } else {
            None
        };
        Ok(Stmt::Import { path, names, alias })
    }

    // ── type ─────────────────────────────────────────────────────────────────

    fn parse_type_decl(&mut self) -> PResult<Stmt> {
        self.advance(); // eat `type`
        let name = self.eat_ident()?;
        self.skip_newlines();
        self.eat_indent()?;

        let mut fields: Vec<FieldDecl>   = Vec::new();
        let mut variants: Vec<VariantDecl> = Vec::new();

        loop {
            self.skip_newlines();
            if self.at_dedent() || self.at_eof() { break; }

            // Peek: uppercase first char → enum variant, else → struct field.
            let member_name = match self.peek().clone() {
                TokenKind::Ident(s) => s,
                _ => return Err(self.err("expected field or variant name")),
            };

            if member_name.chars().next().map_or(false, |c| c.is_uppercase()) {
                // Variant: `Name`  or  `Name(Type, ...)`
                self.advance();
                let variant_fields = if self.at_op(Op::LParen) {
                    self.advance(); // eat '('
                    let mut vf = Vec::new();
                    while !self.at_op(Op::RParen) && !self.at_eof() {
                        // Optional name:
                        let (fname, ftype) = if matches!(self.peek_nth(1), TokenKind::Op(Op2) if *Op2 == Op::Colon) {
                            let n = self.eat_ident()?;
                            self.advance(); // eat ':'
                            (Some(n), self.parse_type_expr()?)
                        } else {
                            (None, self.parse_type_expr()?)
                        };
                        vf.push(VariantField { name: fname, type_ann: ftype });
                        if !self.at_op(Op::RParen) { self.eat_op(Op::Comma)?; }
                    }
                    self.eat_op(Op::RParen)?;
                    vf
                } else {
                    Vec::new()
                };
                variants.push(VariantDecl { name: member_name, fields: variant_fields });
            } else {
                // Field: `name: Type`  or  `name: Type = default`
                self.advance(); // eat name
                self.eat_op(Op::Colon)?;
                let type_ann = self.parse_type_expr()?;
                let default = if self.at_op(Op::Eq) {
                    self.advance();
                    Some(Box::new(self.parse_expr()?))
                } else {
                    None
                };
                fields.push(FieldDecl { name: member_name, type_ann, default });
            }

            if self.at_newline() { self.advance(); }
        }

        self.eat_dedent()?;

        let kind = if !variants.is_empty() {
            TypeDeclKind::Enum(variants)
        } else {
            TypeDeclKind::Struct(fields)
        };
        Ok(Stmt::TypeDecl { name, kind })
    }

    // ── trait ────────────────────────────────────────────────────────────────

    fn parse_trait_decl(&mut self) -> PResult<Stmt> {
        self.advance(); // eat `trait`
        let name = self.eat_ident()?;
        self.skip_newlines();
        self.eat_indent()?;
        let mut methods = Vec::new();
        loop {
            self.skip_newlines();
            if self.at_dedent() || self.at_eof() { break; }
            self.eat_kw(Kw::Fn)?;
            methods.push(self.parse_fn_sig_abstract()?);
            if self.at_newline() { self.advance(); }
        }
        self.eat_dedent()?;
        Ok(Stmt::TraitDecl { name, methods })
    }

    /// Parse a function signature with no body (for trait method declarations).
    fn parse_fn_sig_abstract(&mut self) -> PResult<FnDef> {
        let name = if self.at_ident() { Some(self.eat_ident()?) } else { None };
        let generic_params = if self.at_op(Op::Lt) { self.parse_generic_params()? } else { Vec::new() };
        self.eat_op(Op::LParen)?;
        let params = self.parse_params()?;
        self.eat_op(Op::RParen)?;
        let ret_type = if self.at_op(Op::ThinArrow) {
            self.advance();
            Some(self.parse_type_expr()?)
        } else {
            None
        };
        let where_clause = if self.at_kw(Kw::Where) { self.parse_where_clause()? } else { Vec::new() };
        Ok(FnDef { name, generic_params, params, ret_type, where_clause, body: FnBody::Abstract })
    }

    // ── impl ─────────────────────────────────────────────────────────────────

    fn parse_impl_decl(&mut self) -> PResult<Stmt> {
        self.advance(); // eat `impl`
        let first = self.eat_ident()?;
        // `impl Trait for Type` vs `impl Type`
        let (trait_name, type_name) = if self.at_kw(Kw::For) {
            self.advance();
            (Some(first), self.eat_ident()?)
        } else {
            (None, first)
        };
        self.skip_newlines();
        self.eat_indent()?;
        let mut methods = Vec::new();
        loop {
            self.skip_newlines();
            if self.at_dedent() || self.at_eof() { break; }
            self.eat_kw(Kw::Fn)?;
            methods.push(self.parse_fn_def(false)?);
            if self.at_newline() { self.advance(); }
        }
        self.eat_dedent()?;
        Ok(Stmt::ImplDecl { trait_name, type_name, methods })
    }

    // ── comptime ─────────────────────────────────────────────────────────────

    fn parse_comptime(&mut self) -> PResult<Stmt> {
        self.advance(); // eat `comptime`
        let name = self.eat_ident()?;
        if self.at_op(Op::ColonEq) { self.advance(); }
        else { self.eat_op(Op::Eq)?; }
        let value = self.parse_expr()?;
        Ok(Stmt::Comptime { name, value: Box::new(value) })
    }

    // ── extern fn ────────────────────────────────────────────────────────────

    fn parse_extern_fn(&mut self) -> PResult<Stmt> {
        self.advance(); // eat `extern`
        let lib = if matches!(self.peek(), TokenKind::Str(_)) {
            Some(self.eat_str_lit()?)
        } else {
            None
        };
        self.eat_kw(Kw::Fn)?;
        let name = self.eat_ident()?;
        self.eat_op(Op::LParen)?;
        let params = self.parse_params()?;
        self.eat_op(Op::RParen)?;
        let ret_type = if self.at_op(Op::ThinArrow) {
            self.advance();
            Some(self.parse_type_expr()?)
        } else {
            None
        };
        Ok(Stmt::ExternFn { lib, name, params, ret_type })
    }

    // ── if (statement form) ───────────────────────────────────────────────────

    fn parse_if_stmt(&mut self) -> PResult<Stmt> {
        self.advance(); // eat `if`
        let cond = self.parse_expr()?;
        let then_body = self.parse_stmt_body()?;

        let mut elif_clauses = Vec::new();
        let mut else_body    = None;

        loop {
            // Peek past newlines to find `else` / `elif` (we don't advance yet).
            let saved = self.pos;
            self.skip_newlines();
            if self.at_kw(Kw::Else) {
                self.advance(); // eat `else`
                if self.at_kw(Kw::If) {
                    // elif
                    self.advance(); // eat `if`
                    let ec = self.parse_expr()?;
                    let eb = self.parse_stmt_body()?;
                    elif_clauses.push((Box::new(ec), eb));
                } else {
                    // plain else
                    else_body = Some(self.parse_stmt_body()?);
                    break;
                }
            } else {
                // No else: restore pos (we consumed newlines unnecessarily)
                self.pos = saved;
                break;
            }
        }

        Ok(Stmt::If { cond: Box::new(cond), then_body, elif_clauses, else_body })
    }

    // ── for ──────────────────────────────────────────────────────────────────

    fn parse_for(&mut self) -> PResult<Stmt> {
        self.advance(); // eat `for`
        let var = self.parse_for_var()?;
        self.eat_kw(Kw::In)?;
        let iter = self.parse_expr()?;
        let body = self.parse_stmt_body()?;
        Ok(Stmt::For { var, iter: Box::new(iter), body })
    }

    fn parse_for_var(&mut self) -> PResult<ForVar> {
        // Tuple form: `(a, b)` or `a, b`
        if self.at_op(Op::LParen) {
            self.advance();
            let mut names = vec![self.eat_ident()?];
            while self.at_op(Op::Comma) {
                self.advance();
                if self.at_op(Op::RParen) { break; }
                names.push(self.eat_ident()?);
            }
            self.eat_op(Op::RParen)?;
            return Ok(ForVar::Tuple(names));
        }
        let name = self.eat_ident()?;
        // `a, b in ...` (tuple without parens)
        if self.at_op(Op::Comma) {
            let mut names = vec![name];
            while self.at_op(Op::Comma) {
                self.advance();
                names.push(self.eat_ident()?);
            }
            return Ok(ForVar::Tuple(names));
        }
        Ok(ForVar::Simple(name))
    }

    // ── while ────────────────────────────────────────────────────────────────

    fn parse_while(&mut self) -> PResult<Stmt> {
        self.advance(); // eat `while`
        let cond = self.parse_expr()?;
        let body = self.parse_stmt_body()?;
        Ok(Stmt::While { cond: Box::new(cond), body })
    }

    // ── match (statement form) ────────────────────────────────────────────────

    fn parse_match_stmt(&mut self) -> PResult<Stmt> {
        self.advance(); // eat `match`
        let expr = self.parse_expr()?;
        self.skip_newlines();
        let arms = self.parse_match_arms()?;
        Ok(Stmt::Match { expr: Box::new(expr), arms })
    }

    // ── Assignment / expression statement ─────────────────────────────────────

    /// Typed let: `ident : Type := expr`  or  `ident : Type = expr`
    fn parse_typed_let(&mut self) -> PResult<Stmt> {
        let name = self.eat_ident()?;
        self.eat_op(Op::Colon)?;
        let type_ann = self.parse_type_expr()?;
        let mutable = if self.at_op(Op::ColonEq) {
            self.advance(); false
        } else if self.at_op(Op::Eq) {
            self.advance(); true
        } else {
            return Err(self.err("expected `:=` or `=` after type annotation"));
        };
        let value = self.parse_expr()?;
        Ok(Stmt::Let { name, type_ann: Some(type_ann), mutable, value: Box::new(value) })
    }

    /// True iff `(ident, ...)` or `ident, ident` is followed by `:=`.
    fn looks_like_destructure(&self) -> bool {
        // `(ident, ...) :=` pattern
        if !matches!(self.peek(), TokenKind::Op(Op::LParen)) { return false; }
        let mut i = 1;
        // skip ident tokens and commas inside parens
        loop {
            match self.peek_nth(i) {
                TokenKind::Ident(_) | TokenKind::Op(Op::Comma) => i += 1,
                TokenKind::Op(Op::RParen) => { i += 1; break; }
                _ => return false,
            }
        }
        matches!(self.peek_nth(i), TokenKind::Op(Op::ColonEq))
    }

    fn parse_tuple_destructure(&mut self) -> PResult<Stmt> {
        self.advance(); // eat '('
        let mut names = vec![self.eat_ident()?];
        while self.at_op(Op::Comma) {
            self.advance();
            if self.at_op(Op::RParen) { break; }
            names.push(self.eat_ident()?);
        }
        self.eat_op(Op::RParen)?;
        self.eat_op(Op::ColonEq)?;
        let value = self.parse_expr()?;
        Ok(Stmt::Destructure { names, value: Box::new(value) })
    }

    fn parse_assign_or_expr_stmt(&mut self) -> PResult<Stmt> {
        // Typed let: `ident : Type (:= | =) expr`
        if self.at_ident() && self.peek_nth(1) == &TokenKind::Op(Op::Colon) {
            return self.parse_typed_let();
        }

        // Parse the expression (handles ident, calls, indexing, field access, etc.)
        let expr = self.parse_expr()?;

        // After the expression, check for comma-separated names → destructure
        // e.g. `a, b := pair`
        if self.at_op(Op::Comma) {
            if let Expr::Ident(ref first) = expr {
                let mut names = vec![first.clone()];
                while self.at_op(Op::Comma) {
                    self.advance();
                    names.push(self.eat_ident()?);
                }
                self.eat_op(Op::ColonEq)?;
                let value = self.parse_expr()?;
                return Ok(Stmt::Destructure { names, value: Box::new(value) });
            }
        }

        // Assignment operators
        match self.peek().clone() {
            // Immutable let: `x := expr`
            TokenKind::Op(Op::ColonEq) => {
                self.advance();
                let name = expr_to_ident(expr)
                    .map_err(|_| self.err("left-hand side of `:=` must be a plain identifier"))?;
                let value = self.parse_expr()?;
                Ok(Stmt::Let { name, type_ann: None, mutable: false, value: Box::new(value) })
            }
            // Mutable assign / let: `x = expr`
            TokenKind::Op(Op::Eq) => {
                self.advance();
                let value = self.parse_expr()?;
                let target = expr_to_target(expr)
                    .map_err(|_| self.err("invalid assignment target"))?;
                // If the target is a bare Ident that hasn't been declared yet,
                // the resolver will upgrade this to a mutable let.
                Ok(Stmt::Assign { target, value: Box::new(value) })
            }
            // Compound: `x += expr`
            TokenKind::Op(Op::PlusEq)  => self.finish_compound(expr, CompoundOp::Add),
            TokenKind::Op(Op::MinusEq) => self.finish_compound(expr, CompoundOp::Sub),
            TokenKind::Op(Op::StarEq)  => self.finish_compound(expr, CompoundOp::Mul),
            TokenKind::Op(Op::SlashEq) => self.finish_compound(expr, CompoundOp::Div),
            // Pure expression statement
            _ => Ok(Stmt::Expr(Box::new(expr))),
        }
    }

    fn finish_compound(&mut self, expr: Expr, op: CompoundOp) -> PResult<Stmt> {
        self.advance(); // eat `+=` / `-=` / ...
        let value = self.parse_expr()?;
        let target = expr_to_target(expr)
            .map_err(|_| self.err("invalid compound-assignment target"))?;
        Ok(Stmt::CompoundAssign { target, op, value: Box::new(value) })
    }

    // ═════════════════════════════════════════════════════════════════════════
    // Function definitions
    // ═════════════════════════════════════════════════════════════════════════

    /// Called after `fn` (or `async fn`) has been consumed.
    /// Like `eat_ident` but also allows keyword identifiers used as function
    /// names in the stdlib (Ok, Err, Some, None, etc.).
    fn eat_fn_name(&mut self) -> PResult<String> {
        match self.peek().clone() {
            TokenKind::Ident(s)      => { self.advance(); Ok(s) }
            TokenKind::Kw(Kw::Self_) => { self.advance(); Ok("self".into()) }
            // Allow Ok/Err/Some/None as fn names (stdlib constructors)
            TokenKind::Kw(Kw::Ok)   => { self.advance(); Ok("Ok".into()) }
            TokenKind::Kw(Kw::Err)  => { self.advance(); Ok("Err".into()) }
            TokenKind::Kw(Kw::Some) => { self.advance(); Ok("Some".into()) }
            TokenKind::Nil           => { self.advance(); Ok("None".into()) }
            _ => Err(self.err_expected("identifier")),
        }
    }

    fn at_fn_name(&self) -> bool {
        matches!(self.peek(),
            TokenKind::Ident(_)
            | TokenKind::Kw(Kw::Ok)
            | TokenKind::Kw(Kw::Err)
            | TokenKind::Kw(Kw::Some)
            | TokenKind::Nil
        )
    }

    fn parse_fn_def(&mut self, is_async: bool) -> PResult<FnDef> {
        let name = if self.at_fn_name() { Some(self.eat_fn_name()?) } else { None };
        let generic_params = if self.at_op(Op::Lt) {
            self.parse_generic_params()?
        } else {
            Vec::new()
        };
        self.eat_op(Op::LParen)?;
        let params = self.parse_params()?;
        self.eat_op(Op::RParen)?;
        let ret_type = if self.at_op(Op::ThinArrow) {
            self.advance();
            Some(self.parse_type_expr()?)
        } else {
            None
        };
        let where_clause = if self.at_kw(Kw::Where) {
            self.parse_where_clause()?
        } else {
            Vec::new()
        };
        let body = self.parse_fn_body()?;
        let _ = is_async; // used by caller to select Stmt variant
        Ok(FnDef { name, generic_params, params, ret_type, where_clause, body })
    }

    fn parse_generic_params(&mut self) -> PResult<Vec<GenericParam>> {
        self.eat_op(Op::Lt)?;
        let mut params = Vec::new();
        while !self.at_op(Op::Gt) && !self.at_eof() {
            let name = self.eat_ident()?;
            let bounds = if self.at_op(Op::Colon) {
                self.advance();
                let mut bs = vec![self.eat_ident()?];
                while self.at_op(Op::Plus) {
                    self.advance();
                    bs.push(self.eat_ident()?);
                }
                bs
            } else {
                Vec::new()
            };
            params.push(GenericParam { name, bounds });
            if !self.at_op(Op::Gt) { self.eat_op(Op::Comma)?; }
        }
        self.eat_op(Op::Gt)?;
        Ok(params)
    }

    fn parse_params(&mut self) -> PResult<Vec<Param>> {
        let mut params = Vec::new();
        while !self.at_op(Op::RParen) && !self.at_eof() {
            self.skip_newlines();
            if self.at_op(Op::RParen) { break; }
            params.push(self.parse_param()?);
            if !self.at_op(Op::RParen) {
                self.eat_op(Op::Comma)?;
                self.skip_newlines();
            }
        }
        Ok(params)
    }

    fn parse_param(&mut self) -> PResult<Param> {
        // Variadic: `...name`
        let variadic = if self.at_op(Op::DotDot) {
            // `..` used as spread prefix in params
            self.advance(); true
        } else {
            false
        };
        let name = self.eat_ident()?;
        let type_ann = if self.at_op(Op::Colon) {
            self.advance();
            Some(self.parse_type_expr()?)
        } else {
            None
        };
        let default = if self.at_op(Op::Eq) {
            self.advance();
            Some(Box::new(self.parse_expr()?))
        } else {
            None
        };
        Ok(Param { name, type_ann, default, variadic })
    }

    fn parse_where_clause(&mut self) -> PResult<Vec<(String, Vec<String>)>> {
        self.advance(); // eat `where`
        let mut clauses = Vec::new();
        loop {
            let name = self.eat_ident()?;
            self.eat_op(Op::Colon)?;
            let mut bounds = vec![self.eat_ident()?];
            while self.at_op(Op::Plus) {
                self.advance();
                bounds.push(self.eat_ident()?);
            }
            clauses.push((name, bounds));
            if !self.at_op(Op::Comma) { break; }
            self.advance();
        }
        Ok(clauses)
    }

    // ═════════════════════════════════════════════════════════════════════════
    // Match arms
    // ═════════════════════════════════════════════════════════════════════════

    fn parse_match_arms(&mut self) -> PResult<Vec<MatchArm>> {
        self.eat_indent()?;
        let mut arms = Vec::new();
        loop {
            self.skip_newlines();
            if self.at_dedent() || self.at_eof() { break; }
            let pattern = self.parse_pattern()?;
            let guard = if self.at_kw(Kw::If) {
                self.advance();
                Some(Box::new(self.parse_expr()?))
            } else {
                None
            };
            self.eat_op(Op::FatArrow)?;
            // Body: block or single expression
            let body = if self.at_newline() {
                let saved = self.pos;
                self.advance(); // skip newline
                if self.at_indent() {
                    let stmts = self.parse_block()?;
                    MatchBody::Block(stmts)
                } else {
                    // wasn't a block after all — backtrack and parse as expr
                    self.pos = saved;
                    MatchBody::Expr(Box::new(self.parse_expr()?))
                }
            } else {
                MatchBody::Expr(Box::new(self.parse_expr()?))
            };
            arms.push(MatchArm { pattern, guard, body });
            if self.at_newline() { self.advance(); }
        }
        self.eat_dedent()?;
        Ok(arms)
    }

    // ═════════════════════════════════════════════════════════════════════════
    // Patterns
    // ═════════════════════════════════════════════════════════════════════════

    fn parse_pattern(&mut self) -> PResult<Pattern> {
        let first = self.parse_single_pattern()?;
        // `p1 | p2 | ...`  — OR pattern
        if self.at_op(Op::Pipe) {
            let mut alts = vec![first];
            while self.at_op(Op::Pipe) {
                self.advance();
                alts.push(self.parse_single_pattern()?);
            }
            Ok(Pattern::Or(alts))
        } else {
            Ok(first)
        }
    }

    fn parse_single_pattern(&mut self) -> PResult<Pattern> {
        match self.peek().clone() {
            // Wildcard `_`
            TokenKind::Ident(ref s) if s == "_" => { self.advance(); Ok(Pattern::Wildcard) }
            // Negative integer literal: `-5`
            TokenKind::Op(Op::Minus) => {
                self.advance();
                match self.peek().clone() {
                    TokenKind::Int(n) => {
                        self.advance();
                        // Check for range: `-5..10`
                        if self.at_op(Op::DotDot) || self.at_op(Op::DotDotEq) {
                            let inclusive = self.at_op(Op::DotDotEq);
                            self.advance();
                            let end = if matches!(self.peek(), TokenKind::Int(_) | TokenKind::Op(Op::Minus)) {
                                let sign: i64 = if self.at_op(Op::Minus) { self.advance(); -1 } else { 1 };
                                match self.advance().kind {
                                    TokenKind::Int(e) => Some(sign * e),
                                    _ => return Err(self.err("expected integer in range pattern")),
                                }
                            } else { None };
                            Ok(Pattern::Range { start: -n, end, inclusive })
                        } else {
                            Ok(Pattern::NegInt(-n))
                        }
                    }
                    _ => Err(self.err("expected integer after `-` in pattern")),
                }
            }
            // Integer range or literal
            TokenKind::Int(n) => {
                self.advance();
                if self.at_op(Op::DotDot) || self.at_op(Op::DotDotEq) {
                    let inclusive = self.at_op(Op::DotDotEq);
                    self.advance();
                    let end = if matches!(self.peek(), TokenKind::Int(_)) {
                        let e = match self.advance().kind { TokenKind::Int(x) => x, _ => unreachable!() };
                        Some(e)
                    } else { None };
                    Ok(Pattern::Range { start: n, end, inclusive })
                } else {
                    Ok(Pattern::Literal(Box::new(Expr::Int(n))))
                }
            }
            // Float / String / Bool / Nil literals
            TokenKind::Float(v) => { self.advance(); Ok(Pattern::Literal(Box::new(Expr::Float(v)))) }
            TokenKind::Str(s)   => { self.advance(); Ok(Pattern::Literal(Box::new(Expr::Str(s)))) }
            TokenKind::Bool(b)  => { self.advance(); Ok(Pattern::Literal(Box::new(Expr::Bool(b)))) }
            // `nil` / `None` both lex as TokenKind::Nil
            TokenKind::Nil      => { self.advance(); Ok(Pattern::NonePat) }
            // `Some(p)`
            TokenKind::Kw(Kw::Some) => {
                self.advance();
                self.eat_op(Op::LParen)?;
                let inner = self.parse_pattern()?;
                self.eat_op(Op::RParen)?;
                Ok(Pattern::SomePat(Box::new(inner)))
            }
            // `Ok(p)`
            TokenKind::Kw(Kw::Ok) => {
                self.advance();
                self.eat_op(Op::LParen)?;
                let inner = self.parse_pattern()?;
                self.eat_op(Op::RParen)?;
                Ok(Pattern::OkPat(Box::new(inner)))
            }
            // `Err(p)`
            TokenKind::Kw(Kw::Err) => {
                self.advance();
                self.eat_op(Op::LParen)?;
                let inner = self.parse_pattern()?;
                self.eat_op(Op::RParen)?;
                Ok(Pattern::ErrPat(Box::new(inner)))
            }
            // Identifier: uppercase → Ctor, lowercase → Bind
            TokenKind::Ident(name) => {
                self.advance();
                let is_upper = name.chars().next().map_or(false, |c| c.is_uppercase());
                if is_upper {
                    // `Name.Variant` or `Name(p, ...)`
                    let variant = if self.at_op(Op::Dot) {
                        self.advance();
                        Some(self.eat_ident()?)
                    } else { None };
                    let args = if self.at_op(Op::LParen) {
                        self.advance();
                        let mut ps = Vec::new();
                        while !self.at_op(Op::RParen) && !self.at_eof() {
                            ps.push(self.parse_pattern()?);
                            if !self.at_op(Op::RParen) { self.eat_op(Op::Comma)?; }
                        }
                        self.eat_op(Op::RParen)?;
                        ps
                    } else { Vec::new() };
                    Ok(Pattern::Ctor { name, variant, args })
                } else {
                    Ok(Pattern::Bind(name))
                }
            }
            _ => Err(self.err("expected pattern")),
        }
    }

    // ═════════════════════════════════════════════════════════════════════════
    // Type expressions
    // ═════════════════════════════════════════════════════════════════════════

    fn parse_type_expr(&mut self) -> PResult<TypeExpr> {
        match self.peek().clone() {
            TokenKind::Op(Op::LBracket) => {
                self.advance();
                let inner = self.parse_type_expr()?;
                self.eat_op(Op::RBracket)?;
                Ok(TypeExpr::List(Box::new(inner)))
            }
            TokenKind::Op(Op::Amp) => {
                self.advance();
                Ok(TypeExpr::Ref(Box::new(self.parse_type_expr()?)))
            }
            TokenKind::Op(Op::Quest) => {
                self.advance();
                Ok(TypeExpr::Option(Box::new(self.parse_type_expr()?)))
            }
            TokenKind::Op(Op::LParen) => {
                self.advance();
                let mut types = vec![self.parse_type_expr()?];
                while self.at_op(Op::Comma) {
                    self.advance();
                    if self.at_op(Op::RParen) { break; }
                    types.push(self.parse_type_expr()?);
                }
                self.eat_op(Op::RParen)?;
                Ok(TypeExpr::Tuple(types))
            }
            TokenKind::Ident(name) => {
                self.advance();
                let args = if self.at_op(Op::Lt) {
                    self.advance();
                    let mut ts = vec![self.parse_type_expr()?];
                    while self.at_op(Op::Comma) {
                        self.advance();
                        ts.push(self.parse_type_expr()?);
                    }
                    self.eat_op(Op::Gt)?;
                    ts
                } else { Vec::new() };
                Ok(TypeExpr::Named(name, args))
            }
            // Primitive type keywords used as type expressions
            TokenKind::Kw(kw) => {
                let name = kw.as_str().to_string();
                self.advance();
                Ok(TypeExpr::Named(name, Vec::new()))
            }
            _ => Err(self.err_expected("type expression")),
        }
    }

    // ═════════════════════════════════════════════════════════════════════════
    // Expressions
    // ═════════════════════════════════════════════════════════════════════════

    pub fn parse_expr(&mut self) -> PResult<Expr> {
        self.parse_or_expr()
    }

    // Level: or / ||
    fn parse_or_expr(&mut self) -> PResult<Expr> {
        let mut lhs = self.parse_and_expr()?;
        while matches!(self.peek(), TokenKind::Kw(Kw::Or) | TokenKind::Op(Op::PipePipe)) {
            self.advance();
            let rhs = self.parse_and_expr()?;
            lhs = Expr::BinOp { op: BinOp::Or, lhs: Box::new(lhs), rhs: Box::new(rhs) };
        }
        Ok(lhs)
    }

    // Level: and / &&
    fn parse_and_expr(&mut self) -> PResult<Expr> {
        let mut lhs = self.parse_not_expr()?;
        while matches!(self.peek(), TokenKind::Kw(Kw::And) | TokenKind::Op(Op::AmpAmp)) {
            self.advance();
            let rhs = self.parse_not_expr()?;
            lhs = Expr::BinOp { op: BinOp::And, lhs: Box::new(lhs), rhs: Box::new(rhs) };
        }
        Ok(lhs)
    }

    // Level: not / !
    fn parse_not_expr(&mut self) -> PResult<Expr> {
        if matches!(self.peek(), TokenKind::Kw(Kw::Not) | TokenKind::Op(Op::Bang)) {
            self.advance();
            let expr = self.parse_not_expr()?;
            Ok(Expr::UnOp { op: UnOp::Not, expr: Box::new(expr) })
        } else {
            self.parse_cmp_expr()
        }
    }

    // Level: == != < > <= >= is
    fn parse_cmp_expr(&mut self) -> PResult<Expr> {
        let lhs = self.parse_range_expr()?;
        let op = match self.peek() {
            TokenKind::Op(Op::EqEq)  => BinOp::Eq,
            TokenKind::Op(Op::BangEq)=> BinOp::Ne,
            TokenKind::Op(Op::Lt)    => BinOp::Lt,
            TokenKind::Op(Op::Gt)    => BinOp::Gt,
            TokenKind::Op(Op::LtEq)  => BinOp::Le,
            TokenKind::Op(Op::GtEq)  => BinOp::Ge,
            TokenKind::Kw(Kw::Is)    => BinOp::Is,
            _ => return Ok(lhs),
        };
        self.advance();
        let rhs = self.parse_range_expr()?;
        Ok(Expr::BinOp { op, lhs: Box::new(lhs), rhs: Box::new(rhs) })
    }

    // Level: .. ..=  (non-associative)
    fn parse_range_expr(&mut self) -> PResult<Expr> {
        let lhs = self.parse_add_expr()?;
        if self.at_op(Op::DotDot) || self.at_op(Op::DotDotEq) {
            let inclusive = self.at_op(Op::DotDotEq);
            self.advance();
            let rhs = self.parse_add_expr()?;
            Ok(Expr::Range { start: Box::new(lhs), end: Box::new(rhs), inclusive })
        } else {
            Ok(lhs)
        }
    }

    // Level: + -
    fn parse_add_expr(&mut self) -> PResult<Expr> {
        let mut lhs = self.parse_mul_expr()?;
        loop {
            let op = match self.peek() {
                TokenKind::Op(Op::Plus)  => BinOp::Add,
                TokenKind::Op(Op::Minus) => BinOp::Sub,
                _ => break,
            };
            self.advance();
            let rhs = self.parse_mul_expr()?;
            lhs = Expr::BinOp { op, lhs: Box::new(lhs), rhs: Box::new(rhs) };
        }
        Ok(lhs)
    }

    // Level: * / // %   (operand is pipe, so pipe binds tighter than mul)
    fn parse_mul_expr(&mut self) -> PResult<Expr> {
        let mut lhs = self.parse_pipe_expr()?;
        loop {
            let op = match self.peek() {
                TokenKind::Op(Op::Star)       => BinOp::Mul,
                TokenKind::Op(Op::Slash)      => BinOp::Div,
                TokenKind::Op(Op::SlashSlash) => BinOp::IntDiv,
                TokenKind::Op(Op::Percent)    => BinOp::Mod,
                TokenKind::Op(Op::At)         => BinOp::Matmul,
                _ => break,
            };
            self.advance();
            let rhs = self.parse_pipe_expr()?;
            lhs = Expr::BinOp { op, lhs: Box::new(lhs), rhs: Box::new(rhs) };
        }
        Ok(lhs)
    }

    // Level: |>   (higher than * / so `xs |> sum / n` = `(xs|>sum)/n`)
    fn parse_pipe_expr(&mut self) -> PResult<Expr> {
        let mut lhs = self.parse_pow_expr()?;
        loop {
            if self.at_op(Op::PipeGt) {
                // Same-line  `lhs |> rhs`
                self.advance();
                let rhs = self.parse_postfix_expr()?;
                lhs = Expr::Pipe { lhs: Box::new(lhs), rhs: Box::new(rhs) };
            } else if self.at_newline()
                && matches!(self.peek_nth(1), TokenKind::Indent)
                && matches!(self.peek_nth(2), TokenKind::Op(op) if *op == Op::PipeGt)
            {
                // Multiline pipe continuation:
                //   expr
                //     |> f(...)
                //     |> g(...)
                self.advance(); // Newline
                self.advance(); // Indent
                while self.at_op(Op::PipeGt) {
                    self.advance(); // |>
                    let rhs = self.parse_postfix_expr()?;
                    lhs = Expr::Pipe { lhs: Box::new(lhs), rhs: Box::new(rhs) };
                    self.skip_newlines();
                }
                self.eat_dedent()?;
                break;
            } else {
                break;
            }
        }
        Ok(lhs)
    }

    // Level: **  (right-associative)
    fn parse_pow_expr(&mut self) -> PResult<Expr> {
        let base = self.parse_unary_expr()?;
        if self.at_op(Op::StarStar) {
            self.advance();
            let exp = self.parse_pow_expr()?; // right-assoc: recurse
            Ok(Expr::BinOp { op: BinOp::Pow, lhs: Box::new(base), rhs: Box::new(exp) })
        } else {
            Ok(base)
        }
    }

    // Level: unary -  !  not
    fn parse_unary_expr(&mut self) -> PResult<Expr> {
        match self.peek().clone() {
            TokenKind::Op(Op::Minus) => {
                self.advance();
                let e = self.parse_unary_expr()?;
                Ok(Expr::UnOp { op: UnOp::Neg, expr: Box::new(e) })
            }
            TokenKind::Op(Op::Bang) | TokenKind::Kw(Kw::Not) => {
                self.advance();
                let e = self.parse_unary_expr()?;
                Ok(Expr::UnOp { op: UnOp::Not, expr: Box::new(e) })
            }
            _ => self.parse_postfix_expr(),
        }
    }

    // Level: postfix  .field  .method(args)  [idx]  (args)  ?.field
    fn parse_postfix_expr(&mut self) -> PResult<Expr> {
        let mut node = self.parse_primary()?;

        loop {
            match self.peek().clone() {
                // Field access or method call: `.ident`  or  `.ident(args)`
                TokenKind::Op(Op::Dot) => {
                    // Guard: don't consume `.` if it's the start of `..` range.
                    if self.peek_nth(1) == &TokenKind::Op(Op::Dot) { break; }
                    self.advance(); // eat '.'
                    let field = self.eat_ident()?;
                    if self.at_op(Op::LParen) {
                        self.advance();
                        let (args, kwargs) = self.parse_args()?;
                        self.eat_op(Op::RParen)?;
                        node = Expr::MethodCall { obj: Box::new(node), method: field, args, kwargs };
                    } else {
                        node = Expr::Field { obj: Box::new(node), field };
                    }
                }
                // Optional chain: `?.field`  or  `?.method(args)`
                TokenKind::Op(Op::QuestDot) => {
                    self.advance();
                    let field = self.eat_ident()?;
                    node = Expr::OptChain { obj: Box::new(node), field };
                }
                // Index access: `[expr]`
                TokenKind::Op(Op::LBracket) => {
                    self.advance();
                    let idx = self.parse_expr()?;
                    self.eat_op(Op::RBracket)?;
                    node = Expr::Index { obj: Box::new(node), idx: Box::new(idx) };
                }
                // Function call: `(args)`
                TokenKind::Op(Op::LParen) => {
                    self.advance();
                    let (args, kwargs) = self.parse_args()?;
                    self.eat_op(Op::RParen)?;
                    node = Expr::Call { callee: Box::new(node), args, kwargs };
                }
                _ => break,
            }
        }

        Ok(node)
    }

    // ── Argument list ─────────────────────────────────────────────────────────

    fn parse_args(&mut self) -> PResult<(Vec<Expr>, Vec<(String, Expr)>)> {
        let mut args   = Vec::new();
        let mut kwargs = Vec::new();

        while !self.at_op(Op::RParen) && !self.at_eof() {
            self.skip_ws();
            if self.at_op(Op::RParen) { break; }

            // Keyword argument: `name = expr`
            if self.at_ident() && self.peek_nth(1) == &TokenKind::Op(Op::Eq) {
                let kname = self.eat_ident()?;
                self.advance(); // eat '='
                kwargs.push((kname, self.parse_expr()?));
            } else {
                args.push(self.parse_expr()?);
            }

            if !self.at_op(Op::RParen) {
                self.eat_op(Op::Comma)?;
                self.skip_ws();
            }
        }

        Ok((args, kwargs))
    }

    // ── Primary expressions ───────────────────────────────────────────────────

    fn parse_primary(&mut self) -> PResult<Expr> {
        match self.peek().clone() {
            // ── Literals ──────────────────────────────────────────────────────
            TokenKind::Int(n)   => { self.advance(); Ok(Expr::Int(n)) }
            TokenKind::Float(v) => { self.advance(); Ok(Expr::Float(v)) }
            TokenKind::Str(s)   => { self.advance(); Ok(Expr::Str(s)) }
            TokenKind::Bool(b)  => { self.advance(); Ok(Expr::Bool(b)) }
            TokenKind::Nil      => { self.advance(); Ok(Expr::Nil) }

            // ── Self ──────────────────────────────────────────────────────────
            TokenKind::Kw(Kw::Self_) => { self.advance(); Ok(Expr::Self_) }

            // ── Placeholder lambda: `_`, `_ + 1`, `_.field` ──────────────────
            TokenKind::Ident(ref s) if s == "_" => {
                self.advance();
                // Check for binary tail: `_ + expr`
                let op_tail = match self.peek().clone() {
                    TokenKind::Op(Op::Plus)       => Some(BinOp::Add),
                    TokenKind::Op(Op::Minus)      => Some(BinOp::Sub),
                    TokenKind::Op(Op::Star)       => Some(BinOp::Mul),
                    TokenKind::Op(Op::Slash)      => Some(BinOp::Div),
                    TokenKind::Op(Op::SlashSlash) => Some(BinOp::IntDiv),
                    TokenKind::Op(Op::Percent)    => Some(BinOp::Mod),
                    TokenKind::Op(Op::StarStar)   => Some(BinOp::Pow),
                    _ => None,
                };
                if let Some(op) = op_tail {
                    self.advance();
                    let rhs = self.parse_unary_expr()?;
                    return Ok(Expr::Placeholder(Some(Box::new(PlaceholderOp::Bin(op, Box::new(rhs))))));
                }
                // Field tail: `_.field`
                if self.at_op(Op::Dot) && !matches!(self.peek_nth(1), TokenKind::Op(Op::Dot)) {
                    self.advance(); // eat '.'
                    let field = self.eat_ident()?;
                    return Ok(Expr::Placeholder(Some(Box::new(PlaceholderOp::Field(field)))));
                }
                Ok(Expr::Placeholder(None))
            }

            // ── Identifier (possibly a struct literal) ────────────────────────
            TokenKind::Ident(name) => {
                self.advance();
                // `Name { field: val }` → struct literal when Name starts uppercase
                // and the next token is `{`.
                if name.chars().next().map_or(false, |c| c.is_uppercase())
                    && self.at_op(Op::LBrace)
                {
                    return self.parse_struct_body(name);
                }
                Ok(Expr::Ident(name))
            }

            // ── Lambda: `fn(params) => body` ─────────────────────────────────
            TokenKind::Kw(Kw::Fn) => {
                self.advance();
                let def = self.parse_fn_def(false)?;
                Ok(Expr::Lambda(Box::new(def)))
            }

            // ── Async lambda: `async fn(params) => body` ─────────────────────
            TokenKind::Kw(Kw::Async) => {
                self.advance();
                self.eat_kw(Kw::Fn)?;
                let def = self.parse_fn_def(true)?;
                Ok(Expr::Lambda(Box::new(def)))
            }

            // ── If expression: `if cond => then else => other` ────────────────
            TokenKind::Kw(Kw::If) => self.parse_if_expr(),

            // ── Match expression ──────────────────────────────────────────────
            TokenKind::Kw(Kw::Match) => self.parse_match_expr(),

            // ── await expr ────────────────────────────────────────────────────
            TokenKind::Kw(Kw::Await) => {
                self.advance();
                Ok(Expr::Await(Box::new(self.parse_postfix_expr()?)))
            }

            // ── spawn expr ────────────────────────────────────────────────────
            TokenKind::Kw(Kw::Spawn) => {
                self.advance();
                Ok(Expr::Spawn(Box::new(self.parse_postfix_expr()?)))
            }

            // ── unsafe { } ───────────────────────────────────────────────────
            TokenKind::Kw(Kw::Unsafe) => {
                self.advance();
                let stmts = self.parse_block()?;
                Ok(Expr::Unsafe(stmts))
            }

            // ── List: `[...]` ─────────────────────────────────────────────────
            TokenKind::Op(Op::LBracket) => self.parse_list_expr(),

            // ── Map / Set: `{...}` ────────────────────────────────────────────
            TokenKind::Op(Op::LBrace) => self.parse_map_or_set_expr(),

            // ── Parenthesised expression or tuple: `(...)` ───────────────────
            TokenKind::Op(Op::LParen) => self.parse_paren_or_tuple(),

            // ── Some(x) / Ok(x) / Err(x) as expressions ─────────────────────
            TokenKind::Kw(Kw::Some) => {
                self.advance();
                self.eat_op(Op::LParen)?;
                let inner = self.parse_expr()?;
                self.eat_op(Op::RParen)?;
                // Desugar to `Call(Ident("Some"), [inner])`
                Ok(Expr::Call {
                    callee: Box::new(Expr::Ident("Some".into())),
                    args: vec![inner], kwargs: vec![],
                })
            }
            TokenKind::Kw(Kw::Ok) => {
                self.advance();
                self.eat_op(Op::LParen)?;
                let inner = self.parse_expr()?;
                self.eat_op(Op::RParen)?;
                Ok(Expr::Call {
                    callee: Box::new(Expr::Ident("Ok".into())),
                    args: vec![inner], kwargs: vec![],
                })
            }
            TokenKind::Kw(Kw::Err) => {
                self.advance();
                self.eat_op(Op::LParen)?;
                let inner = self.parse_expr()?;
                self.eat_op(Op::RParen)?;
                Ok(Expr::Call {
                    callee: Box::new(Expr::Ident("Err".into())),
                    args: vec![inner], kwargs: vec![],
                })
            }

            // ── Keywords used as function/variable names in expression context ───
            // `where` is a well-known tensor op; `from`/`as`/`in` can appear as
            // identifiers in some call positions. Allow them as identifiers here.
            TokenKind::Kw(kw @ (
                Kw::Where | Kw::From | Kw::As | Kw::In | Kw::Export | Kw::Loop
            )) => {
                let name = format!("{}", kw); // uses Keyword Display impl
                self.advance();
                Ok(Expr::Ident(name))
            }

            _ => Err(self.err_expected("expression")),
        }
    }

    // ── If expression ─────────────────────────────────────────────────────────

    fn parse_if_expr(&mut self) -> PResult<Expr> {
        self.advance(); // eat `if`
        let cond = self.parse_expr()?;
        self.eat_op(Op::FatArrow)?;
        let then_expr = self.parse_expr()?;

        let mut elif_clauses = Vec::new();
        let mut else_expr    = None;

        // Track whether the else-chain started on an indented continuation block.
        // If so, we must consume the matching Dedent after the chain ends.
        let mut consumed_indent = false;
        loop {
            let saved = self.pos;
            self.skip_newlines();
            // Check for `else` on an indented continuation line
            if self.at_indent() && matches!(self.peek_nth(1), TokenKind::Kw(Kw::Else)) {
                self.advance(); // eat Indent
                consumed_indent = true;
            }
            if self.at_kw(Kw::Else) {
                self.advance();
                if self.at_kw(Kw::If) {
                    self.advance();
                    let ec = self.parse_expr()?;
                    self.eat_op(Op::FatArrow)?;
                    let ee = self.parse_expr()?;
                    elif_clauses.push((Box::new(ec), Box::new(ee)));
                } else {
                    // `=>` is optional before the else-expression
                    if self.at_op(Op::FatArrow) { self.advance(); }
                    else_expr = Some(Box::new(self.parse_expr()?));
                    break;
                }
            } else {
                self.pos = saved;
                break;
            }
        }
        // Consume the matching Dedent if we entered a continuation indent block
        if consumed_indent {
            self.skip_newlines();
            if self.at_dedent() { self.advance(); }
        }

        Ok(Expr::If { cond: Box::new(cond), then_expr: Box::new(then_expr), elif_clauses, else_expr })
    }

    // ── Match expression ──────────────────────────────────────────────────────

    fn parse_match_expr(&mut self) -> PResult<Expr> {
        self.advance(); // eat `match`
        let expr = self.parse_expr()?;
        self.skip_newlines();
        let arms = self.parse_match_arms()?;
        Ok(Expr::Match { expr: Box::new(expr), arms })
    }

    // ── List literal ─────────────────────────────────────────────────────────

    fn parse_list_expr(&mut self) -> PResult<Expr> {
        self.advance(); // eat '['
        let mut items = Vec::new();
        while !self.at_op(Op::RBracket) && !self.at_eof() {
            self.skip_ws();
            if self.at_op(Op::RBracket) { break; }
            items.push(self.parse_expr()?);
            self.skip_ws();
            if !self.at_op(Op::RBracket) { self.eat_op(Op::Comma)?; }
            self.skip_ws();
        }
        self.eat_op(Op::RBracket)?;
        Ok(Expr::List(items))
    }

    // ── Map / Set literal ─────────────────────────────────────────────────────

    fn parse_map_or_set_expr(&mut self) -> PResult<Expr> {
        self.advance(); // eat '{'
        self.skip_ws();

        // Empty braces → empty map
        if self.at_op(Op::RBrace) {
            self.advance();
            return Ok(Expr::Map(Vec::new()));
        }

        // Parse first element
        let first = self.parse_expr()?;
        self.skip_ws();

        if self.at_op(Op::Colon) {
            // Map: `{ key: val, ... }`
            self.advance();
            let first_val = self.parse_expr()?;
            let mut pairs = vec![(first, first_val)];
            while self.at_op(Op::Comma) {
                self.advance();
                self.skip_ws();
                if self.at_op(Op::RBrace) { break; }
                let k = self.parse_expr()?;
                self.eat_op(Op::Colon)?;
                let v = self.parse_expr()?;
                pairs.push((k, v));
                self.skip_ws();
            }
            self.eat_op(Op::RBrace)?;
            Ok(Expr::Map(pairs))
        } else {
            // Set: `{ val, val, ... }`
            let mut items = vec![first];
            while self.at_op(Op::Comma) {
                self.advance();
                self.skip_ws();
                if self.at_op(Op::RBrace) { break; }
                items.push(self.parse_expr()?);
                self.skip_ws();
            }
            self.eat_op(Op::RBrace)?;
            Ok(Expr::Set(items))
        }
    }

    // ── Parenthesised expression or tuple ─────────────────────────────────────

    fn parse_paren_or_tuple(&mut self) -> PResult<Expr> {
        self.advance(); // eat '('
        self.skip_ws();

        if self.at_op(Op::RParen) {
            self.advance();
            return Ok(Expr::Tuple(Vec::new())); // unit / empty tuple
        }

        let first = self.parse_expr()?;
        self.skip_ws();

        if self.at_op(Op::Comma) {
            // Tuple
            let mut items = vec![first];
            while self.at_op(Op::Comma) {
                self.advance();
                self.skip_ws();
                if self.at_op(Op::RParen) { break; }
                items.push(self.parse_expr()?);
                self.skip_ws();
            }
            self.eat_op(Op::RParen)?;
            Ok(Expr::Tuple(items))
        } else {
            // Parenthesised expression (just unwrap)
            self.eat_op(Op::RParen)?;
            Ok(first)
        }
    }

    // ── Struct body `{ field: val, ..spread }` ───────────────────────────────

    fn parse_struct_body(&mut self, name: String) -> PResult<Expr> {
        self.advance(); // eat '{'
        let mut fields = Vec::new();
        while !self.at_op(Op::RBrace) && !self.at_eof() {
            self.skip_ws();
            if self.at_op(Op::RBrace) { break; }
            if self.at_op(Op::DotDot) {
                // Spread: `..base`
                self.advance();
                fields.push(StructField::Spread(Box::new(self.parse_expr()?)));
            } else {
                let fname = self.eat_ident()?;
                let value = if self.at_op(Op::Colon) {
                    self.advance();
                    self.parse_expr()?
                } else {
                    // Shorthand: `{ x }` = `{ x: x }`
                    Expr::Ident(fname.clone())
                };
                fields.push(StructField::Named { name: fname, value: Box::new(value) });
            }
            self.skip_ws();
            if !self.at_op(Op::RBrace) { self.eat_op(Op::Comma)?; }
            self.skip_ws();
        }
        self.eat_op(Op::RBrace)?;
        Ok(Expr::Struct { name, fields })
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Free helpers (no access to Parser state)
// ─────────────────────────────────────────────────────────────────────────────

/// Extract a plain identifier name from an expression, or error.
fn expr_to_ident(e: Expr) -> Result<String, ()> {
    match e {
        Expr::Ident(s) => Ok(s),
        _ => Err(()),
    }
}

/// Convert an expression to an assignment target, or error.
fn expr_to_target(e: Expr) -> Result<AssignTarget, ()> {
    match e {
        Expr::Ident(s) => Ok(AssignTarget::Ident(s)),
        Expr::Index { obj, idx }  => Ok(AssignTarget::Index { obj, idx }),
        Expr::Field { obj, field } => Ok(AssignTarget::Field { obj, field }),
        _ => Err(()),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Unit tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer;

    fn parse_expr(src: &str) -> Expr {
        let tokens = lexer::tokenize(src).expect("lex failed");
        let mut p  = Parser::new(tokens);
        p.parse_expr().expect("parse failed")
    }

    fn parse_stmt(src: &str) -> Stmt {
        let tokens = lexer::tokenize(src).expect("lex failed");
        let mut p  = Parser::new(tokens);
        p.skip_newlines();
        p.parse_stmt().expect("parse failed")
    }

    fn parse_prog(src: &str) -> Program {
        let tokens = lexer::tokenize(src).expect("lex failed");
        parse(tokens).expect("parse failed")
    }

    // ── Literals ─────────────────────────────────────────────────────────────

    #[test]
    fn lit_int() {
        assert!(matches!(parse_expr("42"), Expr::Int(42)));
    }

    #[test]
    fn lit_float() {
        assert!(matches!(parse_expr("3.14"), Expr::Float(_)));
    }

    #[test]
    fn lit_str() {
        assert!(matches!(parse_expr(r#""hello""#), Expr::Str(_)));
    }

    #[test]
    fn lit_bool() {
        assert!(matches!(parse_expr("true"),  Expr::Bool(true)));
        assert!(matches!(parse_expr("false"), Expr::Bool(false)));
    }

    #[test]
    fn lit_nil() {
        assert!(matches!(parse_expr("nil"), Expr::Nil));
    }

    // ── Binary operations ─────────────────────────────────────────────────────

    #[test]
    fn binop_add() {
        let e = parse_expr("1 + 2");
        assert!(matches!(e, Expr::BinOp { op: BinOp::Add, .. }));
    }

    #[test]
    fn binop_precedence() {
        // `1 + 2 * 3` should parse as `1 + (2 * 3)` because mul is not below add;
        // but in Nuvola, the order is add → mul → pipe → pow, so mul calls pipe
        // which is tighter. `2 * 3` => mul(2, 3). `1 + mul(2,3)` => add(1, mul).
        let e = parse_expr("1 + 2 * 3");
        match e {
            Expr::BinOp { op: BinOp::Add, lhs, rhs } => {
                assert!(matches!(*lhs, Expr::Int(1)));
                assert!(matches!(*rhs, Expr::BinOp { op: BinOp::Mul, .. }));
            }
            _ => panic!("expected Add at top level"),
        }
    }

    #[test]
    fn binop_comparison() {
        assert!(matches!(parse_expr("x == y"), Expr::BinOp { op: BinOp::Eq, .. }));
        assert!(matches!(parse_expr("a != b"), Expr::BinOp { op: BinOp::Ne, .. }));
    }

    #[test]
    fn binop_logic() {
        assert!(matches!(parse_expr("a and b"), Expr::BinOp { op: BinOp::And, .. }));
        assert!(matches!(parse_expr("a or b"),  Expr::BinOp { op: BinOp::Or,  .. }));
    }

    #[test]
    fn unop_neg() {
        assert!(matches!(parse_expr("-x"), Expr::UnOp { op: UnOp::Neg, .. }));
    }

    #[test]
    fn unop_not() {
        assert!(matches!(parse_expr("not x"), Expr::UnOp { op: UnOp::Not, .. }));
        assert!(matches!(parse_expr("!x"),    Expr::UnOp { op: UnOp::Not, .. }));
    }

    // ── Pipe ──────────────────────────────────────────────────────────────────

    #[test]
    fn pipe_basic() {
        let e = parse_expr("xs |> sum");
        assert!(matches!(e, Expr::Pipe { .. }));
    }

    #[test]
    fn pipe_tighter_than_mul() {
        // `xs |> sum / n` = `(xs |> sum) / n`
        let e = parse_expr("xs |> sum / n");
        match e {
            Expr::BinOp { op: BinOp::Div, lhs, .. } => {
                assert!(matches!(*lhs, Expr::Pipe { .. }));
            }
            _ => panic!("expected Div at top level, got {:?}", e),
        }
    }

    // ── Range ─────────────────────────────────────────────────────────────────

    #[test]
    fn range_exclusive() {
        let e = parse_expr("0..10");
        assert!(matches!(e, Expr::Range { inclusive: false, .. }));
    }

    #[test]
    fn range_inclusive() {
        let e = parse_expr("1..=5");
        assert!(matches!(e, Expr::Range { inclusive: true, .. }));
    }

    // ── Function calls ────────────────────────────────────────────────────────

    #[test]
    fn call_simple() {
        let e = parse_expr("foo(1, 2)");
        match e {
            Expr::Call { args, .. } => assert_eq!(args.len(), 2),
            _ => panic!("expected Call"),
        }
    }

    #[test]
    fn call_kwargs() {
        let e = parse_expr("f(x, y=1)");
        match e {
            Expr::Call { args, kwargs, .. } => {
                assert_eq!(args.len(), 1);
                assert_eq!(kwargs.len(), 1);
                assert_eq!(kwargs[0].0, "y");
            }
            _ => panic!("expected Call"),
        }
    }

    #[test]
    fn method_call() {
        let e = parse_expr("xs.map(f)");
        assert!(matches!(e, Expr::MethodCall { .. }));
    }

    #[test]
    fn field_access() {
        let e = parse_expr("obj.field");
        assert!(matches!(e, Expr::Field { .. }));
    }

    #[test]
    fn index_access() {
        let e = parse_expr("arr[0]");
        assert!(matches!(e, Expr::Index { .. }));
    }

    // ── Collections ───────────────────────────────────────────────────────────

    #[test]
    fn list_literal() {
        let e = parse_expr("[1, 2, 3]");
        match e {
            Expr::List(items) => assert_eq!(items.len(), 3),
            _ => panic!("expected List"),
        }
    }

    #[test]
    fn map_literal() {
        let e = parse_expr(r#"{"a": 1, "b": 2}"#);
        match e {
            Expr::Map(pairs) => assert_eq!(pairs.len(), 2),
            _ => panic!("expected Map"),
        }
    }

    #[test]
    fn empty_map() {
        assert!(matches!(parse_expr("{}"), Expr::Map(_)));
    }

    #[test]
    fn tuple_literal() {
        let e = parse_expr("(1, 2, 3)");
        match e {
            Expr::Tuple(items) => assert_eq!(items.len(), 3),
            _ => panic!("expected Tuple"),
        }
    }

    // ── Placeholder lambda ────────────────────────────────────────────────────

    #[test]
    fn placeholder_identity() {
        assert!(matches!(parse_expr("_"), Expr::Placeholder(None)));
    }

    #[test]
    fn placeholder_binop() {
        assert!(matches!(parse_expr("_ * 2"), Expr::Placeholder(Some(_))));
    }

    #[test]
    fn placeholder_field() {
        assert!(matches!(parse_expr("_.name"), Expr::Placeholder(Some(_))));
    }

    // ── Lambda ────────────────────────────────────────────────────────────────

    #[test]
    fn lambda_arrow() {
        let e = parse_expr("fn(x) => x + 1");
        assert!(matches!(e, Expr::Lambda(_)));
    }

    // ── If expression ─────────────────────────────────────────────────────────

    #[test]
    fn if_expr_simple() {
        let e = parse_expr("if x > 0 => 1 else => 0");
        match e {
            Expr::If { else_expr, .. } => assert!(else_expr.is_some()),
            _ => panic!("expected If expr"),
        }
    }

    // ── Statements ───────────────────────────────────────────────────────────

    #[test]
    fn stmt_immutable_let() {
        let s = parse_stmt("x := 42");
        match s {
            Stmt::Let { name, mutable, .. } => {
                assert_eq!(name, "x");
                assert!(!mutable);
            }
            _ => panic!("expected Let"),
        }
    }

    #[test]
    fn stmt_mutable_assign() {
        let s = parse_stmt("x = 42");
        assert!(matches!(s, Stmt::Assign { .. }));
    }

    #[test]
    fn stmt_compound_add() {
        let s = parse_stmt("x += 1");
        assert!(matches!(s, Stmt::CompoundAssign { op: CompoundOp::Add, .. }));
    }

    #[test]
    fn stmt_typed_let() {
        let s = parse_stmt("x: i64 := 0");
        match s {
            Stmt::Let { name, type_ann, mutable, .. } => {
                assert_eq!(name, "x");
                assert!(!mutable);
                assert!(type_ann.is_some());
            }
            _ => panic!("expected typed Let"),
        }
    }

    #[test]
    fn stmt_destructure_parens() {
        let s = parse_stmt("(a, b) := pair");
        match s {
            Stmt::Destructure { names, .. } => assert_eq!(names, vec!["a", "b"]),
            _ => panic!("expected Destructure"),
        }
    }

    #[test]
    fn stmt_destructure_comma() {
        let s = parse_stmt("a, b := pair");
        match s {
            Stmt::Destructure { names, .. } => assert_eq!(names, vec!["a", "b"]),
            _ => panic!("expected Destructure"),
        }
    }

    #[test]
    fn stmt_fn_decl_arrow() {
        let s = parse_stmt("fn add(a, b) => a + b");
        match s {
            Stmt::FnDecl(def) => {
                assert_eq!(def.name.as_deref(), Some("add"));
                assert_eq!(def.params.len(), 2);
                assert!(matches!(def.body, FnBody::Arrow(_)));
            }
            _ => panic!("expected FnDecl"),
        }
    }

    #[test]
    fn stmt_fn_decl_block() {
        let src = "fn foo(x)\n  return x\n";
        let s = parse_stmt(src);
        match s {
            Stmt::FnDecl(def) => {
                assert!(matches!(def.body, FnBody::Block(_)));
            }
            _ => panic!("expected FnDecl"),
        }
    }

    #[test]
    fn stmt_return() {
        assert!(matches!(parse_stmt("return 42"), Stmt::Return(Some(_))));
        assert!(matches!(parse_stmt("return"),    Stmt::Return(None)));
    }

    #[test]
    fn stmt_if_block() {
        let src = "if x > 0\n  print(x)\n";
        let s = parse_stmt(src);
        match s {
            Stmt::If { elif_clauses, else_body, .. } => {
                assert!(elif_clauses.is_empty());
                assert!(else_body.is_none());
            }
            _ => panic!("expected If"),
        }
    }

    #[test]
    fn stmt_if_else() {
        let src = "if x > 0\n  print(x)\nelse\n  print(0)\n";
        let s = parse_stmt(src);
        match s {
            Stmt::If { else_body, .. } => assert!(else_body.is_some()),
            _ => panic!("expected If with else"),
        }
    }

    #[test]
    fn stmt_while() {
        let src = "while x > 0\n  x = x - 1\n";
        assert!(matches!(parse_stmt(src), Stmt::While { .. }));
    }

    #[test]
    fn stmt_for_simple() {
        let src = "for i in 0..10\n  print(i)\n";
        match parse_stmt(src) {
            Stmt::For { var: ForVar::Simple(v), .. } => assert_eq!(v, "i"),
            _ => panic!("expected For"),
        }
    }

    #[test]
    fn stmt_for_tuple() {
        let src = "for (i, v) in xs\n  print(i)\n";
        match parse_stmt(src) {
            Stmt::For { var: ForVar::Tuple(vs), .. } => assert_eq!(vs.len(), 2),
            _ => panic!("expected For with tuple var"),
        }
    }

    #[test]
    fn stmt_match() {
        let src = "match x\n  0 => zero\n  _ => other\n";
        match parse_stmt(src) {
            Stmt::Match { arms, .. } => assert_eq!(arms.len(), 2),
            _ => panic!("expected Match"),
        }
    }

    #[test]
    fn stmt_import_basic() {
        let s = parse_stmt("import math");
        assert!(matches!(s, Stmt::Import { .. }));
    }

    #[test]
    fn stmt_import_names() {
        let s = parse_stmt("import math.{sin, cos}");
        match s {
            Stmt::Import { names: Some(ns), .. } => assert_eq!(ns.len(), 2),
            _ => panic!("expected Import with names"),
        }
    }

    #[test]
    fn stmt_comptime() {
        let s = parse_stmt("comptime MAX := 512");
        assert!(matches!(s, Stmt::Comptime { .. }));
    }

    #[test]
    fn stmt_type_struct() {
        let src = "type Point\n  x: f64\n  y: f64\n";
        match parse_stmt(src) {
            Stmt::TypeDecl { kind: TypeDeclKind::Struct(fs), .. } => assert_eq!(fs.len(), 2),
            _ => panic!("expected TypeDecl(Struct)"),
        }
    }

    #[test]
    fn stmt_type_enum() {
        let src = "type Shape\n  Circle(f64)\n  Rect(f64, f64)\n";
        match parse_stmt(src) {
            Stmt::TypeDecl { kind: TypeDeclKind::Enum(vs), .. } => assert_eq!(vs.len(), 2),
            _ => panic!("expected TypeDecl(Enum)"),
        }
    }

    #[test]
    fn stmt_annotation() {
        let s = parse_stmt("@pure\nfn id(x) => x");
        assert!(matches!(s, Stmt::Annotation { .. }));
    }

    // ── Full programs ─────────────────────────────────────────────────────────

    #[test]
    fn prog_hello_world() {
        let stmts = parse_prog(r#"print("Hello, World!")"#);
        assert_eq!(stmts.len(), 1);
        assert!(matches!(stmts[0], Stmt::Expr(_)));
    }

    #[test]
    fn prog_fibonacci() {
        let src = "fn fib(n)\n  if n <= 1\n    return n\n  return fib(n - 1) + fib(n - 2)\n";
        let stmts = parse_prog(src);
        assert_eq!(stmts.len(), 1);
        assert!(matches!(stmts[0], Stmt::FnDecl(_)));
    }

    #[test]
    fn prog_struct_and_method() {
        let src = "type Point\n  x: f64\n  y: f64\n\nfn distance(p)\n  sqrt(p.x * p.x + p.y * p.y)\n";
        let stmts = parse_prog(src);
        assert_eq!(stmts.len(), 2);
    }

    #[test]
    fn prog_comptime_and_fn() {
        let src = "\
comptime MAX := 512\n\
fn cap(x) => if x > MAX => MAX else => x\n";
        let stmts = parse_prog(src);
        assert_eq!(stmts.len(), 2);
        assert!(matches!(stmts[0], Stmt::Comptime { .. }));
    }

    #[test]
    fn prog_pipeline() {
        let src = "result := data |> filter(_ > 0) |> map(_ * 2) |> sum\n";
        let stmts = parse_prog(src);
        assert_eq!(stmts.len(), 1);
        assert!(matches!(stmts[0], Stmt::Let { .. }));
    }
}
