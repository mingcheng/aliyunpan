use std::time::Duration;

/// Default upload chunk size: 512 KiB.
pub const DEFAULT_CHUNK_SIZE: u64 = 512 * 1024;

#[derive(Debug, Clone)]
pub struct Config {
    pub web_url: String,
    pub auth_url: String,
    pub api_url: String,
    pub user_url: String,
    /// Download Referer. The web API requires the legacy domain.
    pub download_referer: String,
    pub api_id: String,
    pub app_id: String,
    pub user_agent: String,
    pub device_name: String,
    pub model_name: String,
    /// Preferred chunk size, increased automatically to keep the part count at or below 10,000.
    pub chunk_size: u64,
    /// Maximum retry count for 429 and 5xx responses.
    pub max_retries: u32,
    /// Retry `n` waits `retry_delay * n`; a server-provided Retry-After takes precedence.
    pub retry_delay: Duration,
    /// Overall JSON request timeout; also used as the read timeout for data transfers.
    pub request_timeout: Duration,
    pub connect_timeout: Duration,
    /// Refresh the access token before its remaining lifetime falls below this margin.
    pub token_refresh_margin: Duration,
    /// Delay between automatically fetched pages to reduce throttling risk.
    pub page_delay: Duration,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            web_url: "https://www.alipan.com".into(),
            auth_url: "https://auth.alipan.com".into(),
            api_url: "https://api.alipan.com".into(),
            user_url: "https://user.alipan.com".into(),
            download_referer: "https://www.aliyundrive.com/".into(),
            api_id: "pJZInNHN2dZWk8qg".into(),
            app_id: "25dzX3vbYqktVxyX".into(),
            user_agent:
                "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/154.0.0.0 Safari/537.36"
                    .into(),
            device_name: "Chrome浏览器".into(),
            model_name: "Windows网页版".into(),
            chunk_size: DEFAULT_CHUNK_SIZE,
            max_retries: 10,
            retry_delay: Duration::from_secs(2),
            request_timeout: Duration::from_secs(60),
            connect_timeout: Duration::from_secs(15),
            token_refresh_margin: Duration::from_secs(300),
            page_delay: Duration::from_millis(100),
        }
    }
}

impl Config {
    /// Use one base URL for auth, API, and user endpoints, for proxies or test servers.
    pub fn with_base_url(base: &str) -> Self {
        let base = base.trim_end_matches('/').to_owned();
        Self {
            auth_url: base.clone(),
            api_url: base.clone(),
            user_url: base,
            ..Self::default()
        }
    }

    pub(crate) fn referer(&self) -> String {
        format!("{}/", self.web_url.trim_end_matches('/'))
    }
}
