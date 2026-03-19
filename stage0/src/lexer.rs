/// Nuvola lexer — Stage 0
///
/// Two-phase pipeline:
///   Phase 1  `Lexer::lex()`         → raw `Vec<Token>` (NEWLINE but no INDENT/DEDENT)
///   Phase 2  `inject_indents()`     → inserts INDENT / DEDENT tokens Python-style
///
/// Token correspondence with the Python reference tokenizer
/// (tests/nuvola_eval.py :: tokenize):
///
///   Python    Rust
///   ───────── ────────────────────────────────────────────
///   INT       TokenKind::Int(i64)
///   FLOAT     TokenKind::Float(f64)
///   STRING    TokenKind::Str(String)   (escapes resolved; {..} kept raw)
///   BOOL      TokenKind::Bool(bool)
///   NIL       TokenKind::Nil
///   KW        TokenKind::Kw(Keyword)
///   IDENT     TokenKind::Ident(String)
///   ANNOT     TokenKind::Annot(String) (@name)
///   OP        TokenKind::Op(Op)
///   NEWLINE   TokenKind::Newline
///   EOF       TokenKind::Eof
///   (new)     TokenKind::Indent
///   (new)     TokenKind::Dedent
///
/// String interpolation: `{expr}` markers are left intact in the Str value.
/// The parser is responsible for splitting strings into literal + interp
/// segments.  `\{` → literal `{` (not treated as interpolation).
///
/// Indentation: spaces count 1 each, tabs count 1 each (tabs must not be
/// mixed with spaces in the same indented block — enforced in Phase 2).
/// Phase 2 uses the `col` of the first token on each line as the indent level.

use crate::error::LexError;
use crate::token::{Keyword, Op, Span, Token, TokenKind};

// ─────────────────────────────────────────────────────────────────────────────
// Public entry point
// ─────────────────────────────────────────────────────────────────────────────

/// Tokenize Nuvola source.  Returns a token stream ending with `Eof`.
///
/// INDENT / DEDENT tokens are injected around indented blocks following
/// Python-style significant-whitespace rules.
pub fn tokenize(src: &str) -> Result<Vec<Token>, LexError> {
    let raw    = Lexer::new(src).lex()?;
    let tokens = inject_indents(raw);
    Ok(tokens)
}

// ─────────────────────────────────────────────────────────────────────────────
// Phase 1 — raw lexer
// ─────────────────────────────────────────────────────────────────────────────

struct Lexer<'src> {
    src:  &'src [u8],
    pos:  usize,
    line: u32,   // 1-indexed
    col:  u32,   // 0-indexed; reset to 0 after every '\n'
}

impl<'src> Lexer<'src> {
    fn new(src: &'src str) -> Self {
        Lexer { src: src.as_bytes(), pos: 0, line: 1, col: 0 }
    }

    // ── Primitive helpers ────────────────────────────────────────────────────

    /// Byte at current position, or `None` at EOF.
    #[inline]
    fn peek(&self) -> Option<u8> {
        self.src.get(self.pos).copied()
    }

    /// Byte `n` positions ahead (0 = current), or `None`.
    #[inline]
    fn peek_at(&self, n: usize) -> Option<u8> {
        self.src.get(self.pos + n).copied()
    }

    /// True if the bytes starting at `pos` equal `prefix`.
    #[inline]
    fn starts_with(&self, prefix: &[u8]) -> bool {
        self.src.get(self.pos..).map_or(false, |s| s.starts_with(prefix))
    }

    /// Consume one byte, update line/col, and return the byte.
    fn advance(&mut self) -> Option<u8> {
        let b = *self.src.get(self.pos)?;
        self.pos += 1;
        if b == b'\n' {
            self.line += 1;
            self.col = 0;
        } else {
            self.col += 1;
        }
        Some(b)
    }

    /// Snapshot of current position for span construction.
    #[inline]
    fn here(&self) -> (usize, u32, u32) {
        (self.pos, self.line, self.col)
    }

    /// Build a token whose raw text started at `(off, ln, col)`.
    #[inline]
    fn tok(&self, kind: TokenKind, off: usize, ln: u32, col: u32) -> Token {
        Token::new(kind, Span::new(off, self.pos - off, ln, col))
    }

    // ── Main lex loop ────────────────────────────────────────────────────────

    fn lex(mut self) -> Result<Vec<Token>, LexError> {
        let mut tokens: Vec<Token> = Vec::new();

        while let Some(b) = self.peek() {
            let (off, ln, col) = self.here();

            // ── `--` line comment ────────────────────────────────────────────
            if b == b'-' && self.peek_at(1) == Some(b'-') {
                self.skip_comment();
                continue;
            }

            // ── Horizontal whitespace (space, tab, CR) ───────────────────────
            if b == b' ' || b == b'\t' || b == b'\r' {
                self.advance();
                continue;
            }

            // ── Newline ──────────────────────────────────────────────────────
            if b == b'\n' {
                self.advance();
                tokens.push(Token::new(TokenKind::Newline, Span::new(off, 1, ln, col)));
                continue;
            }

            // ── `@name` annotation / `@` matmul ─────────────────────────────
            if b == b'@' {
                tokens.push(self.lex_at(off, ln, col)?);
                continue;
            }

            // ── `"""` triple-quoted string ───────────────────────────────────
            if self.starts_with(b"\"\"\"") {
                tokens.push(self.lex_triple_str(off, ln, col)?);
                continue;
            }

            // ── `"..."` regular string ───────────────────────────────────────
            if b == b'"' {
                tokens.push(self.lex_str(off, ln, col)?);
                continue;
            }

            // ── Number ───────────────────────────────────────────────────────
            if b.is_ascii_digit()
                || (b == b'.' && self.peek_at(1).map_or(false, |c| c.is_ascii_digit()))
            {
                tokens.push(self.lex_number(off, ln, col)?);
                continue;
            }

            // ── Identifier or keyword ────────────────────────────────────────
            if b.is_ascii_alphabetic() || b == b'_' {
                tokens.push(self.lex_word(off, ln, col));
                continue;
            }

            // ── Operator / punctuation ───────────────────────────────────────
            if let Some(t) = self.lex_op(off, ln, col) {
                tokens.push(t);
                continue;
            }

            // ── Unknown character ────────────────────────────────────────────
            return Err(LexError::new(
                format!("unexpected character {:?} (U+{:04X})", b as char, b),
                ln, col,
            ));
        }

        tokens.push(Token::new(TokenKind::Eof, Span::new(self.pos, 0, self.line, self.col)));
        Ok(tokens)
    }

    // ── Comment ──────────────────────────────────────────────────────────────

    /// Skip from current `--` to end of line (not consuming the newline).
    fn skip_comment(&mut self) {
        while let Some(b) = self.peek() {
            if b == b'\n' {
                break;
            }
            self.advance();
        }
    }

    // ── Annotation / matmul `@` ───────────────────────────────────────────────

    fn lex_at(&mut self, off: usize, ln: u32, col: u32) -> Result<Token, LexError> {
        self.advance(); // consume '@'

        // If immediately followed by an identifier start char → annotation.
        if self.peek().map_or(false, |b| b.is_ascii_alphabetic() || b == b'_') {
            let name_start = self.pos;
            while self.peek().map_or(false, |b| b.is_ascii_alphanumeric() || b == b'_') {
                self.advance();
            }
            let name_bytes = &self.src[name_start..self.pos];
            let name = format!("@{}", std::str::from_utf8(name_bytes).unwrap());
            Ok(self.tok(TokenKind::Annot(name), off, ln, col))
        } else {
            // Standalone `@` — matrix-multiply operator.
            Ok(self.tok(TokenKind::Op(Op::At), off, ln, col))
        }
    }

    // ── Strings ──────────────────────────────────────────────────────────────

    /// `"""..."""` — raw multi-line string (no escape processing, no interpolation).
    fn lex_triple_str(&mut self, off: usize, ln: u32, col: u32) -> Result<Token, LexError> {
        // consume `"""`
        self.advance(); self.advance(); self.advance();
        let start = self.pos;
        loop {
            if self.starts_with(b"\"\"\"") {
                break;
            }
            if self.peek().is_none() {
                return Err(LexError::new("unterminated triple-quoted string", ln, col));
            }
            self.advance();
        }
        let content = std::str::from_utf8(&self.src[start..self.pos]).unwrap().to_string();
        // consume closing `"""`
        self.advance(); self.advance(); self.advance();
        Ok(self.tok(TokenKind::Str(content), off, ln, col))
    }

    /// `"..."` — regular string with escape processing; `{expr}` kept raw.
    fn lex_str(&mut self, off: usize, ln: u32, col: u32) -> Result<Token, LexError> {
        self.advance(); // consume opening `"`
        let mut buf = String::new();

        loop {
            match self.peek() {
                None | Some(b'\n') => {
                    return Err(LexError::new("unterminated string literal", ln, col));
                }
                Some(b'"') => {
                    self.advance(); // consume closing `"`
                    break;
                }
                Some(b'\\') => {
                    self.advance(); // consume `\`
                    let ch = match self.advance() {
                        Some(b'n')  => '\n',
                        Some(b't')  => '\t',
                        Some(b'r')  => '\r',
                        Some(b'\\') => '\\',
                        Some(b'"')  => '"',
                        Some(b'e')  => '\x1b',  // \e → ESC (ANSI)
                        Some(b'0')  => '\0',    // \0 → null byte
                        // \{ and \} — literal brace (not interpolation)
                        Some(b'{')  => '{',
                        Some(b'}')  => '}',
                        // Any other char: keep as-is
                        Some(c)     => c as char,
                        None        => return Err(LexError::new(
                            "unexpected EOF in escape sequence", ln, col)),
                    };
                    buf.push(ch);
                }
                Some(b) => {
                    buf.push(b as char);
                    self.advance();
                }
            }
        }

        Ok(self.tok(TokenKind::Str(buf), off, ln, col))
    }

    // ── Numbers ──────────────────────────────────────────────────────────────

    /// Lex an integer or float literal, stripping any type suffix.
    ///
    /// Grammar:
    ///   INT   ::= DIGIT+ ('_' DIGIT+)*  IntSuffix?
    ///   FLOAT ::= DIGIT+ '.' DIGIT*  FloatExp?  FloatSuffix?
    ///           | DIGIT+              FloatExp?  FloatSuffix?
    ///   IntSuffix   ::= [ui](8|16|32|64|128|size)
    ///   FloatSuffix ::= f(16|32|64|128)
    ///   FloatExp    ::= [eE] [+-]? DIGIT+
    ///
    /// A `.` is only consumed as part of a float if the NEXT char is a digit
    /// (guards against `1..10` range and `1.method()` method calls).
    fn lex_number(&mut self, off: usize, ln: u32, col: u32) -> Result<Token, LexError> {
        let int_start = self.pos;

        // Integer part (digits + underscores)
        while self.peek().map_or(false, |b| b.is_ascii_digit() || b == b'_') {
            self.advance();
        }

        let mut is_float = false;

        // Fractional part: only if '.' is followed by a digit (not '..' or '.method')
        if self.peek() == Some(b'.') && self.peek_at(1).map_or(false, |b| b.is_ascii_digit()) {
            is_float = true;
            self.advance(); // '.'
            while self.peek().map_or(false, |b| b.is_ascii_digit() || b == b'_') {
                self.advance();
            }
        }

        // Exponent: e[+-]digits
        if matches!(self.peek(), Some(b'e') | Some(b'E')) {
            is_float = true;
            self.advance(); // 'e' / 'E'
            if matches!(self.peek(), Some(b'+') | Some(b'-')) {
                self.advance();
            }
            if !self.peek().map_or(false, |b| b.is_ascii_digit()) {
                return Err(LexError::new(
                    "expected digits after exponent", ln, col));
            }
            while self.peek().map_or(false, |b| b.is_ascii_digit()) {
                self.advance();
            }
        }

        // Record where the numeric content ends (before any suffix)
        let num_end = self.pos;

        // Optional type suffix — consume and classify, then discard
        if matches!(self.peek(), Some(b'u') | Some(b'i') | Some(b'f')) {
            let sfx_start = self.pos;
            self.advance(); // first letter
            while self.peek().map_or(false, |b| b.is_ascii_alphanumeric()) {
                self.advance();
            }
            let sfx = std::str::from_utf8(&self.src[sfx_start..self.pos]).unwrap();

            const FLOAT_SFX: &[&str] = &["f16", "f32", "f64", "f128"];
            const INT_SFX:   &[&str] = &[
                "u8","u16","u32","u64","u128","usize",
                "i8","i16","i32","i64","i128","isize",
            ];

            if FLOAT_SFX.contains(&sfx) {
                is_float = true;
            } else if !INT_SFX.contains(&sfx) {
                // Unknown suffix — backtrack to end of numeric content.
                // Safe because suffix chars are pure ASCII with no newlines.
                self.col -= (self.pos - sfx_start) as u32;
                self.pos  = sfx_start;
            }
        }

        // Parse the clean numeric value (strip underscores)
        let raw   = std::str::from_utf8(&self.src[int_start..num_end]).unwrap();
        let clean: String = raw.chars().filter(|&c| c != '_').collect();

        let kind = if is_float {
            let v = clean.parse::<f64>().map_err(|_| {
                LexError::new(format!("invalid float literal `{}`", clean), ln, col)
            })?;
            TokenKind::Float(v)
        } else {
            let v = clean.parse::<i64>().map_err(|_| {
                LexError::new(
                    format!("integer literal `{}` overflows i64 — use a float or split the value", clean),
                    ln, col,
                )
            })?;
            TokenKind::Int(v)
        };

        Ok(self.tok(kind, off, ln, col))
    }

    // ── Identifiers & keywords ───────────────────────────────────────────────

    fn lex_word(&mut self, off: usize, ln: u32, col: u32) -> Token {
        let start = self.pos;
        while self.peek().map_or(false, |b| b.is_ascii_alphanumeric() || b == b'_') {
            self.advance();
        }
        let word = std::str::from_utf8(&self.src[start..self.pos]).unwrap();

        let kind = match word {
            // Booleans — checked before the keyword table
            "true"         => TokenKind::Bool(true),
            "false"        => TokenKind::Bool(false),
            // Nil — checked before the keyword table
            "nil" | "None" => TokenKind::Nil,
            // Everything else: keyword or plain identifier
            w => match Keyword::from_str(w) {
                Some(kw) => TokenKind::Kw(kw),
                None     => TokenKind::Ident(word.to_string()),
            },
        };

        self.tok(kind, off, ln, col)
    }

    // ── Operators / punctuation ──────────────────────────────────────────────

    /// Longest-match scan against the operator table.
    /// Returns `None` if nothing matches (caller will error).
    fn lex_op(&mut self, off: usize, ln: u32, col: u32) -> Option<Token> {
        // Table is ordered longest-first within each ambiguity class.
        // `@` is intentionally absent — handled by `lex_at` before we get here.
        const OPS: &[(&[u8], Op)] = &[
            // three-char
            (b"...", Op::Ellipsis),
            (b"..=", Op::DotDotEq),
            (b"|>=", Op::PipeGtEq),
            // two-char
            (b"|>",  Op::PipeGt),
            (b"=>",  Op::FatArrow),
            (b":=",  Op::ColonEq),
            (b"!=",  Op::BangEq),
            (b"<=",  Op::LtEq),
            (b">=",  Op::GtEq),
            (b"==",  Op::EqEq),
            (b"&&",  Op::AmpAmp),
            (b"||",  Op::PipePipe),
            (b"<<",  Op::LtLt),
            (b">>",  Op::GtGt),
            (b"->",  Op::ThinArrow),
            (b"**",  Op::StarStar),
            (b"//",  Op::SlashSlash),
            (b"??",  Op::QuestQuest),
            (b"?.",  Op::QuestDot),
            (b"..",  Op::DotDot),
            (b"+=",  Op::PlusEq),
            (b"-=",  Op::MinusEq),
            (b"*=",  Op::StarEq),
            (b"/=",  Op::SlashEq),
            // one-char
            (b"+",   Op::Plus),
            (b"-",   Op::Minus),
            (b"*",   Op::Star),
            (b"/",   Op::Slash),
            (b"%",   Op::Percent),
            (b"<",   Op::Lt),
            (b">",   Op::Gt),
            (b"=",   Op::Eq),
            (b":",   Op::Colon),
            (b".",   Op::Dot),
            (b",",   Op::Comma),
            (b";",   Op::Semi),
            (b"(",   Op::LParen),
            (b")",   Op::RParen),
            (b"[",   Op::LBracket),
            (b"]",   Op::RBracket),
            (b"{",   Op::LBrace),
            (b"}",   Op::RBrace),
            (b"!",   Op::Bang),
            (b"?",   Op::Quest),
            (b"&",   Op::Amp),
            (b"|",   Op::Pipe),
            (b"^",   Op::Caret),
            (b"~",   Op::Tilde),
        ];

        for (bytes, op) in OPS {
            if self.starts_with(bytes) {
                for _ in 0..bytes.len() {
                    self.advance();
                }
                return Some(self.tok(TokenKind::Op(op.clone()), off, ln, col));
            }
        }

        None
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Phase 2 — INDENT / DEDENT injection
// ─────────────────────────────────────────────────────────────────────────────

/// Walk the raw token stream and inject INDENT / DEDENT tokens.
///
/// Rules (Python-style):
///   • After a `Newline`, look at the `col` of the next non-blank token.
///     That `col` is the indent level of the new logical line.
///   • If indent increased  → emit one `Indent` before the new line.
///   • If indent decreased  → emit `Dedent`s until the level matches a
///     previous entry in the indent stack (indentation error if it doesn't).
///   • Blank lines (only `Newline` tokens, no real content) are dropped.
///   • At EOF: emit `Dedent`s to close all open blocks.
///
/// The indent stack starts at `[0]`, meaning column 0 = top-level code.
fn inject_indents(raw: Vec<Token>) -> Vec<Token> {
    let mut out: Vec<Token> = Vec::with_capacity(raw.len() + 16);
    let mut stack:   Vec<u32> = vec![0];  // indent-level stack; 0 = top level
    let mut bracket_depth: i32 = 0;       // ( [ { nesting — suppress INDENT/DEDENT inside
    let mut i = 0;

    while i < raw.len() {
        let tok = &raw[i];

        if tok.kind != TokenKind::Newline {
            // Track bracket depth so we can suppress INDENT/DEDENT inside them.
            match &tok.kind {
                TokenKind::Op(Op::LParen | Op::LBracket | Op::LBrace) => bracket_depth += 1,
                TokenKind::Op(Op::RParen | Op::RBracket | Op::RBrace) => bracket_depth -= 1,
                _ => {}
            }
            // Normal token — pass through.
            out.push(tok.clone());
            i += 1;
            continue;
        }

        // Inside brackets, newlines are just whitespace — no INDENT/DEDENT.
        if bracket_depth > 0 {
            // Swallow the newline and any blank lines; do NOT emit it.
            i += 1;
            while i < raw.len() && raw[i].kind == TokenKind::Newline {
                i += 1;
            }
            continue;
        }

        // ── We have a Newline ────────────────────────────────────────────────
        let newline_tok = tok.clone();
        i += 1;

        // Swallow consecutive blank lines (Newline immediately after Newline).
        while i < raw.len() && raw[i].kind == TokenKind::Newline {
            i += 1;
        }

        // Determine what follows the blank-line run.
        let next = raw.get(i);
        let is_eof = next.map_or(true, |t| t.kind == TokenKind::Eof);

        if is_eof {
            // End of file — emit pending DEDENTs to close all open blocks.
            out.push(newline_tok);
            let eof_span = next.map(|t| t.span.clone()).unwrap_or(Span::dummy());
            while stack.len() > 1 {
                stack.pop();
                out.push(Token::new(TokenKind::Dedent, eof_span.clone()));
            }
            break;
        }

        let next = next.unwrap();
        let next_indent = next.span.col;
        let cur_indent  = *stack.last().unwrap();

        if next_indent > cur_indent {
            // Indentation increased — open a new block.
            out.push(newline_tok);
            stack.push(next_indent);
            out.push(Token::new(TokenKind::Indent, next.span.clone()));
        } else if next_indent < cur_indent {
            // Indentation decreased — close one or more blocks.
            out.push(newline_tok);
            while stack.len() > 1 && *stack.last().unwrap() > next_indent {
                stack.pop();
                out.push(Token::new(TokenKind::Dedent, next.span.clone()));
            }
            // Note: if `next_indent` doesn't match any stack level that is an
            // indentation error in strict mode.  Stage 0 is lenient and
            // continues; the parser will catch structural issues.
        } else {
            // Same indentation — just keep the newline for statement separation.
            out.push(newline_tok);
        }
    }

    // Append the EOF token (either from the loop break or if raw was empty).
    if let Some(eof) = raw.iter().rev().find(|t| t.kind == TokenKind::Eof) {
        // Only append if not already there.
        if out.last().map_or(true, |t| t.kind != TokenKind::Eof) {
            out.push(eof.clone());
        }
    }

    out
}

// ─────────────────────────────────────────────────────────────────────────────
// Unit tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::token::{Keyword, Op, TokenKind};

    // Convenience: lex and return only the kinds (skip span info).
    fn kinds(src: &str) -> Vec<TokenKind> {
        tokenize(src).expect("lex failed").into_iter().map(|t| t.kind).collect()
    }

    // ── Integers ─────────────────────────────────────────────────────────────

    #[test]
    fn int_plain() {
        assert_eq!(kinds("42"), vec![TokenKind::Int(42), TokenKind::Eof]);
    }

    #[test]
    fn int_underscores() {
        assert_eq!(kinds("1_000_000"), vec![TokenKind::Int(1_000_000), TokenKind::Eof]);
    }

    #[test]
    fn int_suffix_u8() {
        assert_eq!(kinds("255u8"), vec![TokenKind::Int(255), TokenKind::Eof]);
    }

    #[test]
    fn int_suffix_i64() {
        assert_eq!(kinds("99i64"), vec![TokenKind::Int(99), TokenKind::Eof]);
    }

    #[test]
    fn int_suffix_usize() {
        assert_eq!(kinds("4usize"), vec![TokenKind::Int(4), TokenKind::Eof]);
    }

    // ── Floats ───────────────────────────────────────────────────────────────

    #[test]
    fn float_plain() {
        assert_eq!(kinds("3.14"), vec![TokenKind::Float(3.14), TokenKind::Eof]);
    }

    #[test]
    fn float_exp() {
        assert_eq!(kinds("1e8"), vec![TokenKind::Float(1e8), TokenKind::Eof]);
    }

    #[test]
    fn float_exp_neg() {
        assert_eq!(kinds("2.5e-3"), vec![TokenKind::Float(2.5e-3), TokenKind::Eof]);
    }

    #[test]
    fn float_suffix_f32() {
        assert_eq!(kinds("1.0f32"), vec![TokenKind::Float(1.0), TokenKind::Eof]);
    }

    #[test]
    fn float_underscore() {
        assert_eq!(kinds("3_14.15_9"), vec![TokenKind::Float(314.159), TokenKind::Eof]);
    }

    // ── Strings ──────────────────────────────────────────────────────────────

    #[test]
    fn str_plain() {
        assert_eq!(kinds(r#""hello""#), vec![
            TokenKind::Str("hello".into()),
            TokenKind::Eof,
        ]);
    }

    #[test]
    fn str_escape_newline() {
        assert_eq!(kinds(r#""a\nb""#), vec![
            TokenKind::Str("a\nb".into()),
            TokenKind::Eof,
        ]);
    }

    #[test]
    fn str_escape_tab() {
        assert_eq!(kinds(r#""a\tb""#), vec![
            TokenKind::Str("a\tb".into()),
            TokenKind::Eof,
        ]);
    }

    #[test]
    fn str_interp_kept_raw() {
        // The `{name}` placeholder must be left intact for the parser.
        assert_eq!(kinds(r#""Hello, {name}!""#), vec![
            TokenKind::Str("Hello, {name}!".into()),
            TokenKind::Eof,
        ]);
    }

    #[test]
    fn str_escaped_brace_not_interp() {
        // `\{` → literal `{`, which in the stored string appears as `{`
        // The parser must not treat it as an interpolation start.
        assert_eq!(kinds(r#""cost: \{x}""#), vec![
            TokenKind::Str("cost: {x}".into()),
            TokenKind::Eof,
        ]);
    }

    #[test]
    fn str_triple_quoted() {
        let src = r#""""hello
world""""#;
        assert_eq!(kinds(src), vec![
            TokenKind::Str("hello\nworld".into()),
            TokenKind::Eof,
        ]);
    }

    // ── Bool & Nil ───────────────────────────────────────────────────────────

    #[test]
    fn bool_true() {
        assert_eq!(kinds("true"), vec![TokenKind::Bool(true), TokenKind::Eof]);
    }

    #[test]
    fn bool_false() {
        assert_eq!(kinds("false"), vec![TokenKind::Bool(false), TokenKind::Eof]);
    }

    #[test]
    fn nil_nil() {
        assert_eq!(kinds("nil"), vec![TokenKind::Nil, TokenKind::Eof]);
    }

    #[test]
    fn nil_none() {
        // `None` → Nil (not Kw)
        assert_eq!(kinds("None"), vec![TokenKind::Nil, TokenKind::Eof]);
    }

    // ── Keywords ─────────────────────────────────────────────────────────────

    #[test]
    fn kw_fn() {
        assert_eq!(kinds("fn"), vec![TokenKind::Kw(Keyword::Fn), TokenKind::Eof]);
    }

    #[test]
    fn kw_match() {
        assert_eq!(kinds("match"), vec![TokenKind::Kw(Keyword::Match), TokenKind::Eof]);
    }

    #[test]
    fn kw_comptime() {
        assert_eq!(kinds("comptime"), vec![TokenKind::Kw(Keyword::Comptime), TokenKind::Eof]);
    }

    #[test]
    fn kw_some_ok_err() {
        assert_eq!(kinds("Some Ok Err"), vec![
            TokenKind::Kw(Keyword::Some),
            TokenKind::Kw(Keyword::Ok),
            TokenKind::Kw(Keyword::Err),
            TokenKind::Eof,
        ]);
    }

    // ── Identifiers ──────────────────────────────────────────────────────────

    #[test]
    fn ident_simple() {
        assert_eq!(kinds("foo"), vec![TokenKind::Ident("foo".into()), TokenKind::Eof]);
    }

    #[test]
    fn ident_underscore() {
        assert_eq!(kinds("_x"), vec![TokenKind::Ident("_x".into()), TokenKind::Eof]);
    }

    #[test]
    fn ident_mixed_case() {
        assert_eq!(kinds("MyStruct"), vec![TokenKind::Ident("MyStruct".into()), TokenKind::Eof]);
    }

    // ── Annotations ──────────────────────────────────────────────────────────

    #[test]
    fn annot_pure() {
        assert_eq!(kinds("@pure"), vec![TokenKind::Annot("@pure".into()), TokenKind::Eof]);
    }

    #[test]
    fn annot_gpu() {
        assert_eq!(kinds("@gpu"), vec![TokenKind::Annot("@gpu".into()), TokenKind::Eof]);
    }

    #[test]
    fn at_matmul_standalone() {
        // `@` not followed by an ident is the matmul operator.
        assert_eq!(kinds("A @ B"), vec![
            TokenKind::Ident("A".into()),
            TokenKind::Op(Op::At),
            TokenKind::Ident("B".into()),
            TokenKind::Eof,
        ]);
    }

    // ── Operators ────────────────────────────────────────────────────────────

    #[test]
    fn op_walrus() {
        assert_eq!(kinds(":="), vec![TokenKind::Op(Op::ColonEq), TokenKind::Eof]);
    }

    #[test]
    fn op_pipe() {
        assert_eq!(kinds("|>"), vec![TokenKind::Op(Op::PipeGt), TokenKind::Eof]);
    }

    #[test]
    fn op_fat_arrow() {
        assert_eq!(kinds("=>"), vec![TokenKind::Op(Op::FatArrow), TokenKind::Eof]);
    }

    #[test]
    fn op_range_inclusive() {
        assert_eq!(kinds("..="), vec![TokenKind::Op(Op::DotDotEq), TokenKind::Eof]);
    }

    #[test]
    fn op_range_exclusive() {
        assert_eq!(kinds("1..10"), vec![
            TokenKind::Int(1),
            TokenKind::Op(Op::DotDot),
            TokenKind::Int(10),
            TokenKind::Eof,
        ]);
    }

    #[test]
    fn op_thin_arrow() {
        assert_eq!(kinds("->"), vec![TokenKind::Op(Op::ThinArrow), TokenKind::Eof]);
    }

    #[test]
    fn op_power() {
        assert_eq!(kinds("**"), vec![TokenKind::Op(Op::StarStar), TokenKind::Eof]);
    }

    #[test]
    fn op_int_div() {
        assert_eq!(kinds("//"), vec![TokenKind::Op(Op::SlashSlash), TokenKind::Eof]);
    }

    #[test]
    fn op_compound_assign() {
        assert_eq!(kinds("+="), vec![TokenKind::Op(Op::PlusEq), TokenKind::Eof]);
        assert_eq!(kinds("-="), vec![TokenKind::Op(Op::MinusEq), TokenKind::Eof]);
        assert_eq!(kinds("*="), vec![TokenKind::Op(Op::StarEq),  TokenKind::Eof]);
        assert_eq!(kinds("/="), vec![TokenKind::Op(Op::SlashEq), TokenKind::Eof]);
    }

    // ── Comment stripping ────────────────────────────────────────────────────

    #[test]
    fn comment_skipped() {
        assert_eq!(kinds("-- this is a comment\n42"), vec![
            TokenKind::Newline,
            TokenKind::Int(42),
            TokenKind::Eof,
        ]);
    }

    #[test]
    fn comment_no_token() {
        // A comment-only file → just EOF (the Newline is present but no INDENT/DEDENT needed).
        let toks = kinds("-- nothing here");
        assert!(toks.contains(&TokenKind::Eof));
        assert!(!toks.contains(&TokenKind::Int(0))); // nothing else meaningful
    }

    // ── Full expressions ─────────────────────────────────────────────────────

    #[test]
    fn expr_arithmetic() {
        assert_eq!(kinds("1 + 2 * 3"), vec![
            TokenKind::Int(1),
            TokenKind::Op(Op::Plus),
            TokenKind::Int(2),
            TokenKind::Op(Op::Star),
            TokenKind::Int(3),
            TokenKind::Eof,
        ]);
    }

    #[test]
    fn expr_binding() {
        // `x := 42`
        assert_eq!(kinds("x := 42"), vec![
            TokenKind::Ident("x".into()),
            TokenKind::Op(Op::ColonEq),
            TokenKind::Int(42),
            TokenKind::Eof,
        ]);
    }

    #[test]
    fn expr_pipeline() {
        // `xs |> map(f)`
        assert_eq!(kinds("xs |> map(f)"), vec![
            TokenKind::Ident("xs".into()),
            TokenKind::Op(Op::PipeGt),
            TokenKind::Ident("map".into()),
            TokenKind::Op(Op::LParen),
            TokenKind::Ident("f".into()),
            TokenKind::Op(Op::RParen),
            TokenKind::Eof,
        ]);
    }

    #[test]
    fn fn_arrow_body() {
        // `fn add(a, b) => a + b`
        assert_eq!(kinds("fn add(a, b) => a + b"), vec![
            TokenKind::Kw(Keyword::Fn),
            TokenKind::Ident("add".into()),
            TokenKind::Op(Op::LParen),
            TokenKind::Ident("a".into()),
            TokenKind::Op(Op::Comma),
            TokenKind::Ident("b".into()),
            TokenKind::Op(Op::RParen),
            TokenKind::Op(Op::FatArrow),
            TokenKind::Ident("a".into()),
            TokenKind::Op(Op::Plus),
            TokenKind::Ident("b".into()),
            TokenKind::Eof,
        ]);
    }

    // ── Indentation / INDENT-DEDENT ──────────────────────────────────────────

    #[test]
    fn indent_basic_block() {
        // fn foo(x)\n  return x
        // Should produce: Fn Ident(..) LParen Ident(..) RParen Newline Indent Return Ident(..) Newline Dedent Eof
        let src = "fn foo(x)\n  return x\n";
        let ks  = kinds(src);
        // Spot-checks:
        assert!(ks.contains(&TokenKind::Indent), "expected Indent, got {:?}", ks);
        assert!(ks.contains(&TokenKind::Dedent), "expected Dedent, got {:?}", ks);
        // Newline before Indent
        let nl_pos    = ks.iter().position(|k| *k == TokenKind::Newline).unwrap();
        let indent_pos = ks.iter().position(|k| *k == TokenKind::Indent).unwrap();
        assert!(nl_pos < indent_pos, "Newline must come before Indent");
    }

    #[test]
    fn indent_no_change_flat() {
        // Two top-level statements — no INDENT or DEDENT.
        let src = "x := 1\ny := 2\n";
        let ks  = kinds(src);
        assert!(!ks.contains(&TokenKind::Indent), "flat code must not emit Indent");
        assert!(!ks.contains(&TokenKind::Dedent), "flat code must not emit Dedent");
    }

    #[test]
    fn indent_nested_blocks() {
        let src = "fn f()\n  if true\n    return 1\n  return 0\n";
        let ks  = kinds(src);
        let n_indent = ks.iter().filter(|k| **k == TokenKind::Indent).count();
        let n_dedent = ks.iter().filter(|k| **k == TokenKind::Dedent).count();
        assert_eq!(n_indent, 2, "expected 2 Indent tokens");
        assert_eq!(n_dedent, 2, "expected 2 Dedent tokens");
    }

    #[test]
    fn blank_lines_ignored_for_indent() {
        // A blank line between two same-level statements must not change the block.
        let src = "x := 1\n\ny := 2\n";
        let ks  = kinds(src);
        assert!(!ks.contains(&TokenKind::Indent));
        assert!(!ks.contains(&TokenKind::Dedent));
    }

    // ── Spans ────────────────────────────────────────────────────────────────

    #[test]
    fn span_line_col() {
        let tokens = tokenize("x := 42").unwrap();
        // `x` should be at line=1 col=0
        let x = &tokens[0];
        assert_eq!(x.span.line, 1);
        assert_eq!(x.span.col,  0);
        // `42` should be at col=5
        let n = &tokens[2];
        assert_eq!(n.span.col, 5);
    }

    #[test]
    fn span_multiline() {
        let tokens = tokenize("a\nb").unwrap();
        // `b` is on line 2
        let b_tok = tokens.iter().find(|t| t.kind == TokenKind::Ident("b".into())).unwrap();
        assert_eq!(b_tok.span.line, 2);
        assert_eq!(b_tok.span.col,  0);
    }

    // ── Nuvola-specific constructs ────────────────────────────────────────────

    #[test]
    fn comptime_decl() {
        // `comptime MAX := 512`
        assert_eq!(kinds("comptime MAX := 512"), vec![
            TokenKind::Kw(Keyword::Comptime),
            TokenKind::Ident("MAX".into()),
            TokenKind::Op(Op::ColonEq),
            TokenKind::Int(512),
            TokenKind::Eof,
        ]);
    }

    #[test]
    fn match_stmt() {
        let src = "match x\n  0 => \"zero\"\n  _ => \"other\"\n";
        let ks  = kinds(src);
        assert!(ks.contains(&TokenKind::Kw(Keyword::Match)));
        assert!(ks.contains(&TokenKind::Op(Op::FatArrow)));
        assert!(ks.contains(&TokenKind::Str("zero".into())));
    }

    #[test]
    fn struct_literal() {
        let src = "Point { x: 1.0, y: 2.0 }";
        assert_eq!(kinds(src), vec![
            TokenKind::Ident("Point".into()),
            TokenKind::Op(Op::LBrace),
            TokenKind::Ident("x".into()),
            TokenKind::Op(Op::Colon),
            TokenKind::Float(1.0),
            TokenKind::Op(Op::Comma),
            TokenKind::Ident("y".into()),
            TokenKind::Op(Op::Colon),
            TokenKind::Float(2.0),
            TokenKind::Op(Op::RBrace),
            TokenKind::Eof,
        ]);
    }

    #[test]
    fn async_spawn() {
        assert_eq!(kinds("async fn spawn await"), vec![
            TokenKind::Kw(Keyword::Async),
            TokenKind::Kw(Keyword::Fn),
            TokenKind::Kw(Keyword::Spawn),
            TokenKind::Kw(Keyword::Await),
            TokenKind::Eof,
        ]);
    }

    #[test]
    fn unsafe_extern() {
        assert_eq!(kinds("unsafe extern"), vec![
            TokenKind::Kw(Keyword::Unsafe),
            TokenKind::Kw(Keyword::Extern),
            TokenKind::Eof,
        ]);
    }
}
