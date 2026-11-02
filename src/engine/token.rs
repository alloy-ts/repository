//! Tokens for the Engine language.

#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    Let,
    Print,
    If,
    Else,
    While,
    Fn,
    Return,
    Identifier(String),
    Number(i64),
    Float(f64),
    StringLit(String),
    Boolean(bool),
    Plus,
    Minus,
    Star,
    Slash,
    Equal,
    EqualEqual,
    BangEqual,
    Less,
    LessEqual,
    Greater,
    GreaterEqual,
    Bang,
    LeftParen,
    RightParen,
    LeftBrace,
    RightBrace,
    Comma,
    Semicolon,
    Eof,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub line: usize,
    pub column: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SpannedToken {
    pub token: Token,
    pub span: Span,
}
