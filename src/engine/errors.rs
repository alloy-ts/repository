use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub line: usize,
    pub column: usize,
}

impl Span {
    pub fn new(line: usize, column: usize) -> Self {
        Self { line, column }
    }
}

impl fmt::Display for Span {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.line, self.column)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ErrorKind {
    Lex,
    Parse,
    Compile,
    Runtime,
    UnknownGlobal,
    UnsupportedType,
    Range,
    DuplicateHost,
    AsyncHost,
    JitUnavailable,
}

impl ErrorKind {
    pub fn code(&self) -> &'static str {
        match self {
            ErrorKind::Lex => "ENGINE_LEX",
            ErrorKind::Parse => "ENGINE_PARSE",
            ErrorKind::Compile => "ENGINE_COMPILE",
            ErrorKind::Runtime => "ENGINE_RUNTIME",
            ErrorKind::UnknownGlobal => "ENGINE_UNKNOWN_GLOBAL",
            ErrorKind::UnsupportedType => "ENGINE_UNSUPPORTED_TYPE",
            ErrorKind::Range => "ENGINE_RANGE",
            ErrorKind::DuplicateHost => "ENGINE_DUPLICATE_HOST",
            ErrorKind::AsyncHost => "ENGINE_ASYNC_HOST",
            ErrorKind::JitUnavailable => "ENGINE_JIT_UNAVAILABLE",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
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
        Self::new(ErrorKind::Lex, message, line, column)
    }

    pub fn parse(message: impl Into<String>, line: usize, column: usize) -> Self {
        Self::new(ErrorKind::Parse, message, line, column)
    }

    pub fn compile(message: impl Into<String>, line: usize, column: usize) -> Self {
        Self::new(ErrorKind::Compile, message, line, column)
    }

    pub fn runtime(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Runtime, message, 0, 0)
    }

    pub fn unknown_global(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::UnknownGlobal, message, 0, 0)
    }

    pub fn unsupported_type(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::UnsupportedType, message, 0, 0)
    }

    pub fn range(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Range, message, 0, 0)
    }

    pub fn duplicate_host(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::DuplicateHost, message, 0, 0)
    }

    pub fn async_host(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::AsyncHost, message, 0, 0)
    }

    pub fn jit_unavailable(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::JitUnavailable, message, 0, 0)
    }
}

impl fmt::Display for EngineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.line > 0 {
            write!(
                f,
                "[{}] {} at {}:{}",
                self.kind.code(),
                self.message,
                self.line,
                self.column
            )
        } else {
            write!(f, "[{}] {}", self.kind.code(), self.message)
        }
    }
}

impl std::error::Error for EngineError {}
