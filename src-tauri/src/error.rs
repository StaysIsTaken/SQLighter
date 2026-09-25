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
        // `{:#}` prints the whole context chain: "connect failed: tls handshake: ..."
        AppError(format!("{e:#}"))
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
