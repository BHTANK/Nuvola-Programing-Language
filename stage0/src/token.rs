use std::fmt;

// ─────────────────────────────────────────────────────────────────────────────
// Span
// ─────────────────────────────────────────────────────────────────────────────

/// Source location of a token: byte offset + human-readable line/col.
///
/// `col` is 0-indexed from the start of the physical line and equals the
/// number of leading whitespace characters — which is the indent level the
/// INDENT/DEDENT pass uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    pub offset: usize,  // byte offset into source
    pub len:    usize,  // byte length of the raw token
    pub line:   u32,    // 1-indexed line number
    pub col:    u32,    // 0-indexed column (= indent depth when first on line)
}

impl Span {
    pub fn new(offset: usize, len: usize, line: u32, col: u32) -> Self {
        Span { offset, len, line, col }
    }
    /// Zero-length sentinel span (for synthesized tokens).
    pub fn dummy() -> Self {
        Span { offset: 0, len: 0, line: 0, col: 0 }
    }
}

impl fmt::Display for Span {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.line, self.col)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Token
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

impl Token {
    pub fn new(kind: TokenKind, span: Span) -> Self {
        Token { kind, span }
    }
}

impl fmt::Display for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:<12} {}", format!("{}:{}", self.span.line, self.span.col), self.kind)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// TokenKind
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum TokenKind {
    // ── Literals ─────────────────────────────────────────────────────────────

    /// Integer literal; type suffix already stripped. e.g. `42`, `1_000`, `255u8`
    Int(i64),
    /// Float literal; type suffix already stripped. e.g. `3.14`, `1e8`, `0.5f32`
    Float(f64),
    /// String content with escape sequences resolved; `{expr}` markers kept raw
    /// for the parser to splice into interpolated segments.
    Str(String),
    /// Boolean literal (`true` / `false`).
    Bool(bool),
    /// Nil value (`nil` / `None`).
    Nil,

    // ── Identifiers & keywords ───────────────────────────────────────────────

    /// Any identifier that is not a reserved word.
    Ident(String),
    /// Reserved keyword.
    Kw(Keyword),

    // ── Annotations ──────────────────────────────────────────────────────────

    /// `@name` decorator — stored as the full string including the `@` prefix.
    Annot(String),

    // ── Operators ────────────────────────────────────────────────────────────

    Op(Op),

    // ── Structural / indentation ─────────────────────────────────────────────

    /// Physical newline character.  Meaningful for statement separation.
    Newline,
    /// Injected by the indent pass when indentation depth increases.
    Indent,
    /// Injected by the indent pass when indentation depth decreases.
    Dedent,
    /// End of file.
    Eof,
}

impl fmt::Display for TokenKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TokenKind::Int(n)    => write!(f, "Int({})", n),
            TokenKind::Float(v)  => write!(f, "Float({})", v),
            TokenKind::Str(s)    => write!(f, "Str({:?})", s),
            TokenKind::Bool(b)   => write!(f, "Bool({})", b),
            TokenKind::Nil       => write!(f, "Nil"),
            TokenKind::Ident(s)  => write!(f, "Ident({})", s),
            TokenKind::Kw(k)     => write!(f, "Kw({})", k),
            TokenKind::Annot(s)  => write!(f, "Annot({})", s),
            TokenKind::Op(op)    => write!(f, "Op({})", op),
            TokenKind::Newline   => write!(f, "Newline"),
            TokenKind::Indent    => write!(f, "Indent"),
            TokenKind::Dedent    => write!(f, "Dedent"),
            TokenKind::Eof       => write!(f, "Eof"),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Keyword
// ─────────────────────────────────────────────────────────────────────────────

/// All reserved words in Nuvola (base + nuclear extensions).
///
/// Note: `true`, `false` → `TokenKind::Bool`; `nil`, `None` → `TokenKind::Nil`.
/// Those are NOT listed here so `Keyword::from_str` never matches them.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Keyword {
    // ── Control flow ─────────────────────────────────────────────────────────
    Fn,
    If,
    Else,
    For,
    While,
    Loop,
    In,
    Return,
    Break,
    Continue,
    Match,

    // ── Declarations ─────────────────────────────────────────────────────────
    Type,
    Trait,
    Impl,
    Import,
    Export,
    From,
    As,
    Where,

    // ── Logic ────────────────────────────────────────────────────────────────
    And,
    Or,
    Not,
    Is,

    // ── Value constructors / patterns ─────────────────────────────────────────
    /// `self` inside impl methods.
    Self_,
    /// `Some(x)` — option constructor / pattern.
    Some,
    /// `Ok(x)` — result constructor / pattern.
    Ok,
    /// `Err(x)` — result constructor / pattern.
    Err,

    // ── Error handling ───────────────────────────────────────────────────────
    Throw,
    Try,
    Catch,

    // ── Nuclear extensions ───────────────────────────────────────────────────
    Comptime,
    Async,
    Await,
    Spawn,
    Extern,
    Unsafe,
}

impl Keyword {
    /// Map a source word to a keyword, or return `None` if it is not reserved.
    /// `true`/`false`/`nil`/`None` are intentionally absent — they produce
    /// `Bool` / `Nil` tokens directly.
    pub fn from_str(s: &str) -> Option<Keyword> {
        match s {
            "fn"       => Some(Keyword::Fn),
            "if"       => Some(Keyword::If),
            "else"     => Some(Keyword::Else),
            "for"      => Some(Keyword::For),
            "while"    => Some(Keyword::While),
            "loop"     => Some(Keyword::Loop),
            "in"       => Some(Keyword::In),
            "return"   => Some(Keyword::Return),
            "break"    => Some(Keyword::Break),
            "continue" => Some(Keyword::Continue),
            "match"    => Some(Keyword::Match),
            "type"     => Some(Keyword::Type),
            "trait"    => Some(Keyword::Trait),
            "impl"     => Some(Keyword::Impl),
            "import"   => Some(Keyword::Import),
            "export"   => Some(Keyword::Export),
            "from"     => Some(Keyword::From),
            "as"       => Some(Keyword::As),
            "where"    => Some(Keyword::Where),
            "and"      => Some(Keyword::And),
            "or"       => Some(Keyword::Or),
            "not"      => Some(Keyword::Not),
            "is"       => Some(Keyword::Is),
            "self"     => Some(Keyword::Self_),
            "Some"     => Some(Keyword::Some),
            "Ok"       => Some(Keyword::Ok),
            "Err"      => Some(Keyword::Err),
            "comptime" => Some(Keyword::Comptime),
            "async"    => Some(Keyword::Async),
            "await"    => Some(Keyword::Await),
            "spawn"    => Some(Keyword::Spawn),
            "extern"   => Some(Keyword::Extern),
            "unsafe"   => Some(Keyword::Unsafe),
            "throw"    => Some(Keyword::Throw),
            "try"      => Some(Keyword::Try),
            "catch"    => Some(Keyword::Catch),
            _          => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Keyword::Fn       => "fn",
            Keyword::If       => "if",
            Keyword::Else     => "else",
            Keyword::For      => "for",
            Keyword::While    => "while",
            Keyword::Loop     => "loop",
            Keyword::In       => "in",
            Keyword::Return   => "return",
            Keyword::Break    => "break",
            Keyword::Continue => "continue",
            Keyword::Match    => "match",
            Keyword::Type     => "type",
            Keyword::Trait    => "trait",
            Keyword::Impl     => "impl",
            Keyword::Import   => "import",
            Keyword::Export   => "export",
            Keyword::From     => "from",
            Keyword::As       => "as",
            Keyword::Where    => "where",
            Keyword::And      => "and",
            Keyword::Or       => "or",
            Keyword::Not      => "not",
            Keyword::Is       => "is",
            Keyword::Self_    => "self",
            Keyword::Some     => "Some",
            Keyword::Ok       => "Ok",
            Keyword::Err      => "Err",
            Keyword::Comptime => "comptime",
            Keyword::Async    => "async",
            Keyword::Await    => "await",
            Keyword::Spawn    => "spawn",
            Keyword::Extern   => "extern",
            Keyword::Unsafe   => "unsafe",
            Keyword::Throw    => "throw",
            Keyword::Try      => "try",
            Keyword::Catch    => "catch",
        }
    }
}

impl fmt::Display for Keyword {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Op
// ─────────────────────────────────────────────────────────────────────────────

/// Every operator / punctuation token in Nuvola, ordered longest-first within
/// each ambiguity group.  The lexer tries them in this order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    // ── Three-character ───────────────────────────────────────────────────────
    Ellipsis,     // ...
    DotDotEq,     // ..=
    PipeGtEq,     // |>=

    // ── Two-character ─────────────────────────────────────────────────────────
    PipeGt,       // |>
    FatArrow,     // =>
    ColonEq,      // :=
    BangEq,       // !=
    LtEq,         // <=
    GtEq,         // >=
    EqEq,         // ==
    AmpAmp,       // &&
    PipePipe,     // ||
    LtLt,         // <<
    GtGt,         // >>
    ThinArrow,    // ->
    StarStar,     // **
    SlashSlash,   // //
    QuestQuest,   // ??
    QuestDot,     // ?.
    DotDot,       // ..
    PlusEq,       // +=
    MinusEq,      // -=
    StarEq,       // *=
    SlashEq,      // /=

    // ── One-character ─────────────────────────────────────────────────────────
    Plus,         // +
    Minus,        // -
    Star,         // *
    Slash,        // /
    Percent,      // %
    Lt,           // <
    Gt,           // >
    Eq,           // =
    Colon,        // :
    Dot,          // .
    Comma,        // ,
    Semi,         // ;
    LParen,       // (
    RParen,       // )
    LBracket,     // [
    RBracket,     // ]
    LBrace,       // {
    RBrace,       // }
    Bang,         // !
    Quest,        // ?
    Amp,          // &
    Pipe,         // |
    Caret,        // ^
    Tilde,        // ~
    /// `@` as matrix-multiply operator  (A @ B).
    /// Note: `@name` annotations are `TokenKind::Annot`, not this variant.
    At,           // @
}

impl Op {
    pub fn as_str(&self) -> &'static str {
        match self {
            Op::Ellipsis    => "...",
            Op::DotDotEq    => "..=",
            Op::PipeGtEq    => "|>=",
            Op::PipeGt      => "|>",
            Op::FatArrow    => "=>",
            Op::ColonEq     => ":=",
            Op::BangEq      => "!=",
            Op::LtEq        => "<=",
            Op::GtEq        => ">=",
            Op::EqEq        => "==",
            Op::AmpAmp      => "&&",
            Op::PipePipe    => "||",
            Op::LtLt        => "<<",
            Op::GtGt        => ">>",
            Op::ThinArrow   => "->",
            Op::StarStar    => "**",
            Op::SlashSlash  => "//",
            Op::QuestQuest  => "??",
            Op::QuestDot    => "?.",
            Op::DotDot      => "..",
            Op::PlusEq      => "+=",
            Op::MinusEq     => "-=",
            Op::StarEq      => "*=",
            Op::SlashEq     => "/=",
            Op::Plus        => "+",
            Op::Minus       => "-",
            Op::Star        => "*",
            Op::Slash       => "/",
            Op::Percent     => "%",
            Op::Lt          => "<",
            Op::Gt          => ">",
            Op::Eq          => "=",
            Op::Colon       => ":",
            Op::Dot         => ".",
            Op::Comma       => ",",
            Op::Semi        => ";",
            Op::LParen      => "(",
            Op::RParen      => ")",
            Op::LBracket    => "[",
            Op::RBracket    => "]",
            Op::LBrace      => "{",
            Op::RBrace      => "}",
            Op::Bang        => "!",
            Op::Quest       => "?",
            Op::Amp         => "&",
            Op::Pipe        => "|",
            Op::Caret       => "^",
            Op::Tilde       => "~",
            Op::At          => "@",
        }
    }
}

impl fmt::Display for Op {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
