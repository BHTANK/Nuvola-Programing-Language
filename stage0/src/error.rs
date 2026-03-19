use std::fmt;

// ─────────────────────────────────────────────────────────────────────────────
// LexError
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct LexError {
    pub msg:  String,
    pub line: u32,
    pub col:  u32,
}

impl LexError {
    pub fn new(msg: impl Into<String>, line: u32, col: u32) -> Self {
        LexError { msg: msg.into(), line, col }
    }
}

impl fmt::Display for LexError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}: {}", self.line, self.col, self.msg)
    }
}

impl std::error::Error for LexError {}

// ─────────────────────────────────────────────────────────────────────────────
// ParseError
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ParseError {
    pub msg:  String,
    pub line: u32,
    pub col:  u32,
}

impl ParseError {
    pub fn new(msg: impl Into<String>, line: u32, col: u32) -> Self {
        ParseError { msg: msg.into(), line, col }
    }
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}: {}", self.line, self.col, self.msg)
    }
}

impl std::error::Error for ParseError {}
