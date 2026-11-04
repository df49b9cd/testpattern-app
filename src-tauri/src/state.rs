use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;

use rusqlite::Connection;

use crate::db::Db;
use crate::error::{Error, Result};

pub const DEFAULT_USER_AGENT: &str = concat!("testpattern/", env!("CARGO_PKG_VERSION"));

pub struct App {
    pub db: Db,
    pub http: reqwest::Client,
    pub cache_dir: PathBuf,
    /// Sources with a sync currently running.
    pub syncing: Mutex<HashSet<i64>>,
}

pub type AppState = Arc<App>;

/// Runs `f` on the blocking pool with a reader connection. Async commands go
/// through this (or `write_blocking`): the database locks are plain mutexes,
/// and the writer is held for a whole catalog sync, so taking them on the
/// async runtime would stall its worker threads meanwhile.
pub async fn read_blocking<T: Send + 'static>(
    st: &AppState,
    f: impl FnOnce(&Connection) -> Result<T> + Send + 'static,
) -> Result<T> {
    let st = st.clone();
    run_blocking(move || f(&st.db.read())).await?
}

/// `read_blocking` with the writer connection.
pub async fn write_blocking<T: Send + 'static>(
    st: &AppState,
    f: impl FnOnce(&mut Connection) -> Result<T> + Send + 'static,
) -> Result<T> {
    let st = st.clone();
    run_blocking(move || f(&mut st.db.write())).await?
}

/// `spawn_blocking` with a join failure (a panic in `f`) as an `Error`.
pub async fn run_blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Result<T> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| Error::msg(e.to_string()))
}

pub fn http_client(user_agent: Option<&str>) -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent(
            user_agent
                .filter(|u| !u.trim().is_empty())
                .unwrap_or(DEFAULT_USER_AGENT),
        )
        .connect_timeout(Duration::from_secs(12))
        .pool_idle_timeout(Duration::from_secs(60))
        .build()
        .expect("http client")
}
