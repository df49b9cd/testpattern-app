use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;

use crate::db::Db;

pub const DEFAULT_USER_AGENT: &str = concat!("testpattern/", env!("CARGO_PKG_VERSION"));

pub struct App {
    pub db: Db,
    pub http: reqwest::Client,
    pub cache_dir: PathBuf,
    /// Sources with a sync currently running.
    pub syncing: Mutex<HashSet<i64>>,
}

pub type AppState = Arc<App>;

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
