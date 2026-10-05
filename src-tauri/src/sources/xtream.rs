//! Xtream Codes API client (player_api.php et al.) with mirror failover.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use serde::Serialize;
use serde_json::Value;
use url::Url;

use crate::error::{Error, Result};
use crate::util::json::{f64_of, i64_of, str_of};

pub struct Xtream {
    bases: Vec<String>,
    current: AtomicUsize,
    username: String,
    password: String,
    client: reqwest::Client,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountInfo {
    pub status: String,
    pub message: Option<String>,
    pub expires_at: Option<i64>,
    pub created_at: Option<i64>,
    pub is_trial: bool,
    pub active_connections: i64,
    pub max_connections: i64,
    pub formats: Vec<String>,
    pub server_timezone: Option<String>,
    /// Server local time minus UTC, in seconds (needed for timeshift URLs).
    pub server_utc_offset: Option<i64>,
    /// The base url that answered.
    pub base_url: String,
}

/// Strips endpoints/queries from whatever the user pasted: "host:port",
/// "http://host/player_api.php?...", "http://host/get.php?..." → "http://host".
pub fn normalize_base(input: &str) -> String {
    let mut s = input.trim().to_owned();
    if !s.contains("://") {
        s = format!("http://{s}");
    }
    match Url::parse(&s) {
        Ok(mut u) => {
            let mut path = u.path().trim_end_matches('/').to_owned();
            for endpoint in [
                "/player_api.php",
                "/get.php",
                "/xmltv.php",
                "/panel_api.php",
                "/c",
            ] {
                if let Some(p) = path.strip_suffix(endpoint) {
                    path = p.to_owned();
                }
            }
            u.set_path(&path);
            u.set_query(None);
            u.set_fragment(None);
            u.to_string().trim_end_matches('/').to_owned()
        }
        Err(_) => s.trim_end_matches('/').to_owned(),
    }
}

/// Recognizes Xtream-style playlist links (get.php?username=..&password=..).
pub fn parse_playlist_url(input: &str) -> Option<(String, String, String)> {
    let u = Url::parse(input.trim()).ok()?;
    if !u.path().ends_with("/get.php") {
        return None;
    }
    let mut user = None;
    let mut pass = None;
    for (k, v) in u.query_pairs() {
        match k.as_ref() {
            "username" => user = Some(v.into_owned()),
            "password" => pass = Some(v.into_owned()),
            _ => {}
        }
    }
    Some((normalize_base(input), user?, pass?))
}

impl Xtream {
    pub fn new(
        base: &str,
        alt: &[String],
        username: &str,
        password: &str,
        client: reqwest::Client,
    ) -> Self {
        let mut bases = vec![normalize_base(base)];
        for a in alt {
            let a = normalize_base(a);
            if !a.is_empty() && !bases.contains(&a) {
                bases.push(a);
            }
        }
        Xtream {
            bases,
            current: AtomicUsize::new(0),
            username: username.to_owned(),
            password: password.to_owned(),
            client,
        }
    }

    pub fn base(&self) -> &str {
        &self.bases[self.current.load(Ordering::Relaxed) % self.bases.len()]
    }

    /// The same stream URL on every other configured server.
    pub fn alternates_for(&self, url: &str) -> Vec<String> {
        let current = self.base();
        match url.strip_prefix(current) {
            Some(path) => self
                .bases
                .iter()
                .filter(|b| *b != current)
                .map(|b| format!("{b}{path}"))
                .collect(),
            None => Vec::new(),
        }
    }

    fn enc(s: &str) -> String {
        url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
    }

    fn api_url(&self, base: &str, action: Option<&str>, extra: &[(&str, String)]) -> String {
        let mut url = format!(
            "{base}/player_api.php?username={}&password={}",
            Self::enc(&self.username),
            Self::enc(&self.password)
        );
        if let Some(a) = action {
            url.push_str("&action=");
            url.push_str(a);
        }
        for (k, v) in extra {
            url.push('&');
            url.push_str(k);
            url.push('=');
            url.push_str(&Self::enc(v));
        }
        url
    }

    /// GETs an API endpoint, failing over to mirrors on network/5xx errors.
    async fn get(
        &self,
        action: Option<&str>,
        extra: &[(&str, String)],
        timeout: Duration,
    ) -> Result<Value> {
        let start = self.current.load(Ordering::Relaxed);
        let mut last_err = None;
        for i in 0..self.bases.len() {
            let idx = (start + i) % self.bases.len();
            let url = self.api_url(&self.bases[idx], action, extra);
            let res = self.client.get(&url).timeout(timeout).send().await;
            match res {
                Ok(r) if r.status().is_success() => {
                    let bytes = r.bytes().await?;
                    let v: Value = serde_json::from_slice(&bytes).map_err(|e| {
                        let head =
                            String::from_utf8_lossy(&bytes[..bytes.len().min(120)]).into_owned();
                        Error::msg(format!("unexpected response from server ({e}): {head}"))
                    })?;
                    if idx != start {
                        log::info!("xtream: switched to mirror {}", self.bases[idx]);
                        self.current.store(idx, Ordering::Relaxed);
                    }
                    return Ok(v);
                }
                Ok(r) if r.status().is_server_error() => {
                    last_err = Some(Error::msg(format!("server error {}", r.status())));
                }
                Ok(r) => {
                    return Err(Error::msg(format!(
                        "server refused request ({})",
                        r.status()
                    )));
                }
                Err(e) => last_err = Some(e.into()),
            }
        }
        Err(last_err.unwrap_or_else(|| Error::msg("no server configured")))
    }

    pub async fn account(&self) -> Result<AccountInfo> {
        let v = self.get(None, &[], Duration::from_secs(20)).await?;
        let ui = v.get("user_info").cloned().unwrap_or(Value::Null);
        let si = v.get("server_info").cloned().unwrap_or(Value::Null);
        if i64_of(&ui["auth"]) != Some(1) {
            return Err(Error::msg("Login failed — check the username and password"));
        }
        let status = str_of(&ui["status"]).unwrap_or_else(|| "Unknown".into());
        let offset = match (str_of(&si["time_now"]), i64_of(&si["timestamp_now"])) {
            (Some(t), Some(ts)) => chrono::NaiveDateTime::parse_from_str(&t, "%Y-%m-%d %H:%M:%S")
                .ok()
                .map(|local| {
                    // round to the nearest quarter hour to absorb request latency
                    let raw = local.and_utc().timestamp() - ts;
                    (raw as f64 / 900.0).round() as i64 * 900
                }),
            _ => None,
        };
        Ok(AccountInfo {
            status,
            message: str_of(&ui["message"]).filter(|m| !m.is_empty()),
            expires_at: i64_of(&ui["exp_date"]),
            created_at: i64_of(&ui["created_at"]),
            is_trial: i64_of(&ui["is_trial"]) == Some(1),
            active_connections: i64_of(&ui["active_cons"]).unwrap_or(0),
            max_connections: i64_of(&ui["max_connections"]).unwrap_or(1),
            formats: ui["allowed_output_formats"]
                .as_array()
                .map(|a| a.iter().filter_map(str_of).collect())
                .unwrap_or_default(),
            server_timezone: str_of(&si["timezone"]),
            server_utc_offset: offset,
            base_url: self.base().to_owned(),
        })
    }

    pub async fn list(&self, action: &str) -> Result<Vec<Value>> {
        let v = self
            .get(Some(action), &[], Duration::from_secs(180))
            .await?;
        Ok(match v {
            Value::Array(a) => a,
            // some panels return {} for empty lists
            _ => Vec::new(),
        })
    }

    pub async fn vod_info(&self, id: &str) -> Result<Value> {
        self.get(
            Some("get_vod_info"),
            &[("vod_id", id.to_owned())],
            Duration::from_secs(30),
        )
        .await
    }

    pub async fn series_info(&self, id: &str) -> Result<Value> {
        self.get(
            Some("get_series_info"),
            &[("series_id", id.to_owned())],
            Duration::from_secs(30),
        )
        .await
    }

    pub async fn short_epg(&self, stream_id: &str, limit: u32) -> Result<Value> {
        self.get(
            Some("get_short_epg"),
            &[
                ("stream_id", stream_id.to_owned()),
                ("limit", limit.to_string()),
            ],
            Duration::from_secs(20),
        )
        .await
    }

    pub fn xmltv_url(&self) -> String {
        format!(
            "{}/xmltv.php?username={}&password={}",
            self.base(),
            Self::enc(&self.username),
            Self::enc(&self.password)
        )
    }

    fn path_creds(&self) -> String {
        format!(
            "{}/{}",
            Self::enc(&self.username),
            Self::enc(&self.password)
        )
    }

    pub fn live_url(&self, id: &str, ext: &str) -> String {
        format!("{}/live/{}/{id}.{ext}", self.base(), self.path_creds())
    }

    pub fn movie_url(&self, id: &str, ext: &str) -> String {
        format!("{}/movie/{}/{id}.{ext}", self.base(), self.path_creds())
    }

    pub fn episode_url(&self, id: &str, ext: &str) -> String {
        format!("{}/series/{}/{id}.{ext}", self.base(), self.path_creds())
    }

    /// Catch-up URL. `start_local` is the programme start in *server* local
    /// time; `minutes` its duration.
    pub fn timeshift_url(
        &self,
        id: &str,
        start_local: chrono::NaiveDateTime,
        minutes: i64,
    ) -> String {
        format!(
            "{}/timeshift/{}/{minutes}/{}/{id}.ts",
            self.base(),
            self.path_creds(),
            start_local.format("%Y-%m-%d:%H-%M")
        )
    }
}

/// Lenient accessors for rating fields that come as "7.3", 7.3 or "".
pub fn rating_of(v: &Value) -> Option<f64> {
    f64_of(v).filter(|r| *r > 0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_bases() {
        assert_eq!(
            normalize_base("cf.example.com:8080"),
            "http://cf.example.com:8080"
        );
        assert_eq!(
            normalize_base("http://h.tv/player_api.php?username=a&password=b"),
            "http://h.tv"
        );
        assert_eq!(normalize_base("https://h.tv/"), "https://h.tv");
        assert_eq!(
            normalize_base("http://h.tv/get.php?username=a&password=b&type=m3u_plus"),
            "http://h.tv"
        );
    }

    #[test]
    fn detects_playlist_links() {
        let (base, u, p) = parse_playlist_url(
            "http://h.tv/get.php?username=ab&password=cd&type=m3u_plus&output=ts",
        )
        .unwrap();
        assert_eq!(
            (base.as_str(), u.as_str(), p.as_str()),
            ("http://h.tv", "ab", "cd")
        );
        assert!(parse_playlist_url("http://h.tv/list.m3u").is_none());
    }
}
