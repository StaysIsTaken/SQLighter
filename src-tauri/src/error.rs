//! Error type returned by all Tauri commands. Serialized as a plain message string.

use serde::{Serialize, Serializer};

#[derive(Debug)]
pub struct AppError(pub String);

pub type AppResult<T> = Result<T, AppError>;

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for AppError {}

impl Serialize for AppError {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0)
    }
}

impl From<anyhow::Error> for AppError {
    fn from(e: anyhow::Error) -> Self {
        AppError(chain_message(&e))
    }
}

/// Joins the context chain ("connect failed: tls handshake: ..."), skipping causes whose text
/// already appears earlier - many drivers repeat their source error in their own message.
pub fn chain_message(e: &anyhow::Error) -> String {
    let mut out = String::new();
    for cause in e.chain() {
        let s = cause.to_string();
        if s.is_empty() || out.contains(&s) {
            continue;
        }
        if !out.is_empty() {
            out.push_str(": ");
        }
        out.push_str(&s);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chain_skips_repeated_causes() {
        let io = std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "Connection refused (os error 61)");
        let e = anyhow::Error::new(io).context("Input/output error: Connection refused (os error 61)").context("connect");
        assert_eq!(chain_message(&e), "connect: Input/output error: Connection refused (os error 61)");
    }
}

macro_rules! from_err {
    ($($t:ty),*) => {$(
        impl From<$t> for AppError {
            fn from(e: $t) -> Self { AppError(e.to_string()) }
        }
    )*};
}

from_err!(
    std::io::Error,
    serde_json::Error,
    tokio_postgres::Error,
    mysql_async::Error,
    rusqlite::Error,
    tiberius::error::Error,
    tauri::Error,
    reqwest::Error
);

#[macro_export]
macro_rules! app_err {
    ($($arg:tt)*) => { $crate::error::AppError(format!($($arg)*)) };
}
