use std::fmt;
use thiserror::Error;

/// Standardized error codes for database operations across backends.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DbErrorCode {
    UniqueViolation,
    ForeignKeyViolation,
    NotFound,
    Busy,
    InvalidInput,
    Internal,
}

impl DbErrorCode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::UniqueViolation => "UNIQUE_VIOLATION",
            Self::ForeignKeyViolation => "FOREIGN_KEY_VIOLATION",
            Self::NotFound => "NOT_FOUND",
            Self::Busy => "BUSY",
            Self::InvalidInput => "INVALID_INPUT",
            Self::Internal => "INTERNAL",
        }
    }
}

impl fmt::Display for DbErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Structured database runtime error with an error code and message.
#[derive(Debug, Error)]
#[error("[{code}] {message}")]
pub struct DbError {
    pub code: DbErrorCode,
    pub message: String,
}

impl DbError {
    pub fn new(code: DbErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub fn unique_violation(message: impl Into<String>) -> Self {
        Self::new(DbErrorCode::UniqueViolation, message)
    }

    pub fn foreign_key_violation(message: impl Into<String>) -> Self {
        Self::new(DbErrorCode::ForeignKeyViolation, message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(DbErrorCode::NotFound, message)
    }

    pub fn busy(message: impl Into<String>) -> Self {
        Self::new(DbErrorCode::Busy, message)
    }

    pub fn invalid_input(message: impl Into<String>) -> Self {
        Self::new(DbErrorCode::InvalidInput, message)
    }

    pub fn from_diesel(err: diesel::result::Error) -> Self {
        use diesel::result::DatabaseErrorKind;
        match err {
            diesel::result::Error::NotFound => Self::not_found("Record not found"),
            diesel::result::Error::DatabaseError(kind, info) => match kind {
                DatabaseErrorKind::UniqueViolation => Self::unique_violation(info.message()),
                DatabaseErrorKind::ForeignKeyViolation => Self::foreign_key_violation(info.message()),
                _ => {
                    let msg = info.message();
                    if msg.to_lowercase().contains("busy") || msg.to_lowercase().contains("locked") {
                        Self::busy(msg)
                    } else {
                        Self::new(DbErrorCode::Internal, msg)
                    }
                }
            },
            other => Self::new(DbErrorCode::Internal, other.to_string()),
        }
    }

    pub fn to_napi(&self) -> napi::Error {
        napi::Error::new(napi::Status::GenericFailure, self.to_string())
    }
}
