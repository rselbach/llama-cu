use std::fmt;

use serde::Serialize;

/// Stable, machine-readable error categories reported to clients.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    InvalidArgument,
    PermissionDenied,
    AppNotFound,
    AppNotRunning,
    AppBlocked,
    NoAppSelected,
    WindowNotFound,
    ElementNotFound,
    StaleElement,
    Unsupported,
    BackgroundUnavailable,
    Timeout,
    Platform,
    Io,
}

/// An error with a stable code and a human-readable message.
#[derive(Debug, Serialize)]
pub struct Error {
    pub code: ErrorCode,
    pub message: String,
}

/// Result type used throughout llama-cu.
pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    /// Creates an error with the given code and message.
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(err: std::io::Error) -> Self {
        Self::new(ErrorCode::Io, err.to_string())
    }
}

impl From<serde_json::Error> for Error {
    fn from(err: serde_json::Error) -> Self {
        Self::new(ErrorCode::Io, err.to_string())
    }
}
