use serde::{Serialize, Serializer};

/// Error type for everything the UI can call. Serialized as a plain message.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Msg(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("database: {0}")]
    Db(#[from] rusqlite::Error),
    /// Network failures. Never carries the request URL: Xtream URLs embed
    /// the account credentials.
    #[error("{0}")]
    Network(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid data: {0}")]
    Json(#[from] serde_json::Error),
    #[error("{0}")]
    Tauri(#[from] tauri::Error),
}

impl From<reqwest::Error> for Error {
    fn from(e: reqwest::Error) -> Self {
        let msg = if e.is_timeout() {
            "The server did not respond in time".to_owned()
        } else if e.is_connect() {
            "Can’t connect to the server — check the address or try a backup server".to_owned()
        } else if e.is_decode() || e.is_body() {
            "The server sent an incomplete or invalid response".to_owned()
        } else if let Some(status) = e.status() {
            format!("The server answered {status}")
        } else {
            format!("Network error: {}", e.without_url())
        };
        Error::Network(msg)
    }
}

impl Error {
    pub fn msg(m: impl Into<String>) -> Self {
        Error::Msg(m.into())
    }
}

impl Serialize for Error {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
