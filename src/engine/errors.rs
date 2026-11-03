use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ErrorKind {
    LexerError,
    ParseError,
    CompileError,
    RuntimeError,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EngineError {
    pub kind: ErrorKind,
    pub message: String,
    pub line: usize,
    pub column: usize,
}

impl EngineError {
    pub fn new(kind: ErrorKind, message: impl Into<String>, line: usize, column: usize) -> Self {
        Self {
            kind,
            message: message.into(),
            line,
            column,
        }
    }

    pub fn lex(message: impl Into<String>, line: usize, column: usize) -> Self {
        Self::new(ErrorKind::LexerError, message, line, column)
    }

    pub fn parse(message: impl Into<String>, line: usize, column: usize) -> Self {
        Self::new(ErrorKind::ParseError, message, line, column)
    }

    pub fn compile(message: impl Into<String>, line: usize, column: usize) -> Self {
        Self::new(ErrorKind::CompileError, message, line, column)
    }

    pub fn runtime(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::RuntimeError, message, 0, 0)
    }

    pub fn code(&self) -> &'static str {
        match self.kind {
            ErrorKind::LexerError | ErrorKind::ParseError | ErrorKind::CompileError => "COMPILE_ERROR",
            ErrorKind::RuntimeError => "RUNTIME_ERROR",
        }
    }
}

impl fmt::Display for EngineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:?} at line {}, column {}: {}",
            self.kind, self.line, self.column, self.message
        )
    }
}

impl std::error::Error for EngineError {}
