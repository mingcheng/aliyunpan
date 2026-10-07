use std::{fmt, sync::Arc, time::Duration};

use bytes::Bytes;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::json;
use tokio::sync::{Mutex, RwLock};

use crate::{
    auth::{Credentials, TokenResponse, TokenStore},
    config::Config,
    error::{ApiError, ApiErrorKind, Error, Result, check_response, decode},
    signature::DeviceKey,
    util,
};

pub(crate) type Headers = Vec<(&'static str, String)>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Host {
    Auth,
    Api,
    User,
}

/// Aliyun Drive web API client. Cheap to clone and share across tasks through internal `Arc` storage.
#[derive(Clone)]
pub struct Client {
    inner: Arc<Inner>,
}

struct Inner {
    http: reqwest::Client,
    config: Config,
    store: Arc<dyn TokenStore>,
    state: RwLock<State>,
    /// Serialize token refresh and session recovery to prevent refresh token rotation conflicts.
    recover: Mutex<()>,
}

struct State {
    creds: Credentials,
    pending_save: bool,
    key: Option<DeviceKey>,
    signature: String,
    nonce: u64,
    epoch: u64,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct SessionResult {
    pub result: bool,
    pub success: bool,
    pub code: Option<String>,
    pub message: Option<String>,
}

impl SessionResult {
    pub(crate) fn into_result(self) -> Result<()> {
        if self.result && self.success {
            return Ok(());
        }
        Err(Error::Api(ApiError {
            code: self
                .code
                .filter(|c| !c.is_empty())
                .unwrap_or_else(|| "SessionRejected".into()),
            message: self.message.unwrap_or_default(),
            display_message: None,
            http_status: 200,
        }))
    }
}

impl fmt::Debug for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Client")
            .field("config", &self.inner.config)
            .finish_non_exhaustive()
    }
}

impl Client {
    /// Load credentials from `store`, refresh the token if needed, and create a device session.
    pub async fn connect<S: TokenStore + 'static>(config: Config, store: S) -> Result<Self> {
        Self::connect_shared(config, Arc::new(store)).await
    }

    pub async fn connect_shared(config: Config, store: Arc<dyn TokenStore>) -> Result<Self> {
        let client = Self::from_store(config, store)?;
        {
            let _guard = client.inner.recover.lock().await;
            client.create_session_locked().await?;
        }
        Ok(client)
    }

    /// Refresh and persist credentials without creating or renewing a device session.
    /// This always refreshes; callers may check `Credentials::is_expired` first.
    /// As with `connect`, a persistence error does not return a usable client.
    pub async fn refresh_credentials<S: TokenStore + 'static>(config: Config, store: S) -> Result<Credentials> {
        let client = Self::from_store(config, Arc::new(store))?;
        client.force_refresh().await?;
        Ok(client.credentials().await)
    }

    fn from_store(config: Config, store: Arc<dyn TokenStore>) -> Result<Self> {
        let mut creds = store.load()?.ok_or(Error::NotLoggedIn)?;
        if creds.refresh_token.is_empty() {
            return Err(Error::NotLoggedIn);
        }
        if creds.device_id.is_empty() {
            creds.device_id = util::random_device_id()?;
            store.save(&creds)?;
        }
        let http = reqwest::Client::builder()
            .user_agent(config.user_agent.as_str())
            .connect_timeout(config.connect_timeout)
            .read_timeout(config.request_timeout)
            .build()?;
        let client = Self {
            inner: Arc::new(Inner {
                http,
                config,
                store,
                state: RwLock::new(State {
                    creds,
                    pending_save: false,
                    key: None,
                    signature: String::new(),
                    nonce: 0,
                    epoch: 0,
                }),
                recover: Mutex::new(()),
            }),
        };
        Ok(client)
    }

    pub fn config(&self) -> &Config {
        &self.inner.config
    }

    pub async fn credentials(&self) -> Credentials {
        self.inner.state.read().await.creds.clone()
    }

    pub async fn user_id(&self) -> String {
        self.inner.state.read().await.creds.user_id.clone()
    }

    /// ID of the default backup drive.
    pub async fn default_drive_id(&self) -> String {
        self.inner.state.read().await.creds.default_drive_id.clone()
    }

    /// Return a valid access token, refreshing it first if necessary.
    pub async fn access_token(&self) -> Result<String> {
        self.ensure_fresh().await?;
        Ok(self.inner.state.read().await.creds.access_token.clone())
    }

    /// Refresh the access token immediately and persist the rotated refresh token.
    pub async fn force_refresh(&self) -> Result<()> {
        let _guard = self.inner.recover.lock().await;
        self.refresh_locked().await
    }

    /// Generate a new device key and call `create_session`.
    pub async fn recreate_session(&self) -> Result<()> {
        let _guard = self.inner.recover.lock().await;
        self.create_session_locked().await
    }

    /// Renew using the current device key and the next nonce, publishing the new
    /// signature only on success. Failure invalidates the local session; the next
    /// signed request recreates it rather than replaying an ambiguous renewal.
    pub async fn renew_session(&self) -> Result<()> {
        let _guard = self.inner.recover.lock().await;
        self.save_pending_locked().await?;
        if self
            .inner
            .state
            .read()
            .await
            .creds
            .is_expired(self.inner.config.token_refresh_margin)
        {
            self.refresh_locked().await?;
        }
        if self.inner.state.read().await.key.is_none() {
            self.create_session_locked().await?;
        }
        let cfg = &self.inner.config;
        let mut state = self.inner.state.write().await;
        let nonce = state
            .nonce
            .checked_add(1)
            .ok_or_else(|| Error::Crypto("session nonce exhausted".into()))?;
        let key = state
            .key
            .take()
            .ok_or_else(|| Error::Crypto("session key unavailable".into()))?;
        // Also protects cancellation: once a renewal is attempted, the old
        // signature cannot safely be reused if the response is lost.
        state.signature.clear();
        state.epoch += 1;
        let signature = key.sign(&cfg.app_id, &state.creds.device_id, &state.creds.user_id, nonce)?;
        let headers = [
            ("authorization", state.creds.authorization()),
            ("x-device-id", state.creds.device_id.clone()),
            ("x-signature", signature.clone()),
        ];
        let bytes = self
            .execute_with_retries(
                Host::Api,
                "/users/v1/users/device/renew_session",
                &to_payload(&json!({}))?,
                &headers,
                0,
            )
            .await?;
        decode::<SessionResult>(&bytes)?.into_result()?;
        state.key = Some(key);
        state.signature = signature;
        state.nonce = nonce;
        Ok(())
    }

    async fn ensure_fresh(&self) -> Result<()> {
        let pending_save = self.inner.state.read().await.pending_save;
        if pending_save {
            let _guard = self.inner.recover.lock().await;
            self.save_pending_locked().await?;
        }
        let seen = {
            let s = self.inner.state.read().await;
            if !s.creds.is_expired(self.inner.config.token_refresh_margin) {
                return Ok(());
            }
            s.creds.access_token.clone()
        };
        self.refresh_if_current(&seen).await
    }

    async fn refresh_if_current(&self, seen: &str) -> Result<()> {
        let _guard = self.inner.recover.lock().await;
        self.save_pending_locked().await?;
        if self.inner.state.read().await.creds.access_token != seen {
            return Ok(());
        }
        self.refresh_locked().await
    }

    async fn refresh_locked(&self) -> Result<()> {
        self.save_pending_locked().await?;
        let refresh_token = self.inner.state.read().await.creds.refresh_token.clone();
        let body = to_payload(&json!({
            "refresh_token": refresh_token,
            "api_id": self.inner.config.api_id,
            "grant_type": "refresh_token",
        }))?;
        let bytes = self.execute(Host::Auth, "/v2/account/token", &body, &[]).await?;
        let token: TokenResponse = decode(&bytes)?;
        if token.access_token.is_empty() || token.user_id.is_empty() {
            return Err(Error::InvalidInput(
                "token response lacks access_token or user_id".into(),
            ));
        }
        let mut s = self.inner.state.write().await;
        if s.creds.user_id != token.user_id {
            s.key = None;
            s.signature.clear();
        }
        s.creds.apply(token);
        s.pending_save = true;
        self.inner.store.save(&s.creds)?;
        s.pending_save = false;
        Ok(())
    }

    async fn save_pending_locked(&self) -> Result<()> {
        let mut s = self.inner.state.write().await;
        if s.pending_save {
            self.inner.store.save(&s.creds)?;
            s.pending_save = false;
        }
        Ok(())
    }

    async fn rebuild_session_if(&self, seen_epoch: u64) -> Result<()> {
        let _guard = self.inner.recover.lock().await;
        if self.inner.state.read().await.epoch != seen_epoch {
            return Ok(());
        }
        self.create_session_locked().await
    }

    async fn create_session_locked(&self) -> Result<()> {
        self.save_pending_locked().await?;
        let cfg = &self.inner.config;
        let mut refreshed = false;
        if self.inner.state.read().await.creds.is_expired(cfg.token_refresh_margin) {
            self.refresh_locked().await?;
            refreshed = true;
        }
        loop {
            let key = DeviceKey::generate()?;
            let (headers, signature) = {
                let s = self.inner.state.read().await;
                let signature = key.sign(&cfg.app_id, &s.creds.device_id, &s.creds.user_id, 0)?;
                let headers = vec![
                    ("authorization", s.creds.authorization()),
                    ("x-device-id", s.creds.device_id.clone()),
                    ("x-signature", signature.clone()),
                ];
                (headers, signature)
            };
            let body = to_payload(&json!({
                "deviceName": cfg.device_name,
                "modelName": cfg.model_name,
                "pubKey": key.public_key_hex(),
            }))?;
            match self
                .execute(Host::Api, "/users/v1/users/device/create_session", &body, &headers)
                .await
            {
                Ok(bytes) => {
                    decode::<SessionResult>(&bytes)?.into_result()?;
                    let mut s = self.inner.state.write().await;
                    s.key = Some(key);
                    s.signature = signature;
                    s.nonce = 0;
                    s.epoch += 1;
                    return Ok(());
                }
                Err(e) if !refreshed && e.api_kind() == Some(ApiErrorKind::AccessTokenInvalid) => {
                    self.refresh_locked().await?;
                    refreshed = true;
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Send a JSON POST without authentication recovery, retrying 429 and 5xx responses with backoff.
    async fn execute(&self, host: Host, path: &str, body: &Bytes, headers: &[(&'static str, String)]) -> Result<Bytes> {
        self.execute_with_retries(host, path, body, headers, self.inner.config.max_retries)
            .await
    }

    async fn execute_with_retries(
        &self,
        host: Host,
        path: &str,
        body: &Bytes,
        headers: &[(&'static str, String)],
        max_retries: u32,
    ) -> Result<Bytes> {
        let cfg = &self.inner.config;
        let base = match host {
            Host::Auth => &cfg.auth_url,
            Host::Api => &cfg.api_url,
            Host::User => &cfg.user_url,
        };
        let url = format!("{}{path}", base.trim_end_matches('/'));
        let referer = cfg.referer();
        let mut attempt = 0;
        loop {
            let mut req = self
                .inner
                .http
                .post(&url)
                .timeout(cfg.request_timeout)
                .header("accept", "application/json, text/plain, */*")
                .header("referer", &referer)
                .header("origin", cfg.web_url.as_str())
                .header("content-type", "application/json;charset=UTF-8")
                .body(body.clone());
            for (name, value) in headers {
                req = req.header(*name, value.as_str());
            }
            let resp = req.send().await?;
            let status = resp.status().as_u16();
            let retry_after = retry_after(&resp);
            let bytes = resp.bytes().await?;
            if is_transient(status) && attempt < max_retries {
                attempt += 1;
                tokio::time::sleep(backoff(cfg.retry_delay, attempt, retry_after)).await;
                continue;
            }
            check_response(status, &bytes, retry_after)?;
            return Ok(bytes);
        }
    }

    /// Authenticated JSON request with automatic token refresh and signed session recovery.
    pub(crate) async fn request<R: DeserializeOwned>(
        &self,
        host: Host,
        path: &str,
        body: &impl Serialize,
        sign: bool,
        extra: &[(&'static str, String)],
    ) -> Result<R> {
        let payload = to_payload(body)?;
        let mut token_retried = false;
        let mut session_retried = false;
        loop {
            self.ensure_fresh().await?;
            if sign {
                let s = self.inner.state.read().await;
                if s.key.is_none() {
                    let epoch = s.epoch;
                    drop(s);
                    self.rebuild_session_if(epoch).await?;
                }
            }
            let (headers, seen_token, epoch) = {
                let s = self.inner.state.read().await;
                let mut headers: Headers = vec![("authorization", s.creds.authorization())];
                if sign {
                    headers.push(("x-device-id", s.creds.device_id.clone()));
                    headers.push(("x-signature", s.signature.clone()));
                }
                headers.extend_from_slice(extra);
                (headers, s.creds.access_token.clone(), s.epoch)
            };
            match self.execute(host, path, &payload, &headers).await {
                Ok(bytes) => return decode(&bytes),
                Err(e) if !token_retried && e.api_kind() == Some(ApiErrorKind::AccessTokenInvalid) => {
                    token_retried = true;
                    self.refresh_if_current(&seen_token).await?;
                }
                Err(e) if sign && !session_retried && e.api_kind() == Some(ApiErrorKind::SignatureInvalid) => {
                    session_retried = true;
                    self.rebuild_session_if(epoch).await?;
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Call a signed endpoint on the configured API host.
    pub(crate) async fn post<R: DeserializeOwned>(&self, path: &str, body: &impl Serialize) -> Result<R> {
        self.request(Host::Api, path, body, true, &[]).await
    }

    pub(crate) async fn post_unsigned<R: DeserializeOwned>(
        &self,
        host: Host,
        path: &str,
        body: &impl Serialize,
    ) -> Result<R> {
        self.request(host, path, body, false, &[]).await
    }

    /// Call a presigned object-storage URL without authorization, retrying 429, 5xx, and 509 responses.
    pub(crate) async fn send_transfer(
        &self,
        build: impl Fn(&reqwest::Client) -> reqwest::RequestBuilder,
    ) -> Result<reqwest::Response> {
        let cfg = &self.inner.config;
        let mut attempt = 0;
        loop {
            // Only replay transfer GETs and PUTs of the same immutable part.
            // JSON mutations use execute(), where timeouts must remain ambiguous.
            let resp = match build(&self.inner.http).send().await {
                Ok(resp) => resp,
                Err(error) if attempt < cfg.max_retries && (error.is_timeout() || error.is_connect()) => {
                    attempt += 1;
                    tokio::time::sleep(backoff(cfg.retry_delay, attempt, None)).await;
                    continue;
                }
                Err(error) => return Err(error.into()),
            };
            let status = resp.status().as_u16();
            if (is_transient(status) || status == 509) && attempt < cfg.max_retries {
                attempt += 1;
                tokio::time::sleep(backoff(cfg.retry_delay, attempt, retry_after(&resp))).await;
                continue;
            }
            return Ok(resp);
        }
    }
}

fn to_payload(body: &impl Serialize) -> Result<Bytes> {
    serde_json::to_vec(body)
        .map(Bytes::from)
        .map_err(|e| Error::InvalidInput(format!("serialize request: {e}")))
}

fn is_transient(status: u16) -> bool {
    matches!(status, 429 | 502 | 503 | 504)
}

fn retry_after(resp: &reqwest::Response) -> Option<Duration> {
    resp.headers()
        .get("retry-after")?
        .to_str()
        .ok()?
        .trim()
        .parse()
        .ok()
        .map(Duration::from_secs)
}

pub(crate) fn backoff(base: Duration, attempt: u32, retry_after: Option<Duration>) -> Duration {
    retry_after
        .unwrap_or(base.saturating_mul(attempt))
        .min(Duration::from_secs(60))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_is_linear_and_capped() {
        let base = Duration::from_secs(2);
        assert_eq!(backoff(base, 1, None), Duration::from_secs(2));
        assert_eq!(backoff(base, 3, None), Duration::from_secs(6));
        assert_eq!(backoff(base, 100, None), Duration::from_secs(60));
        assert_eq!(backoff(base, 1, Some(Duration::from_secs(5))), Duration::from_secs(5));
    }

    #[test]
    fn session_result() {
        assert!(
            SessionResult {
                result: true,
                success: true,
                ..Default::default()
            }
            .into_result()
            .is_ok()
        );
        let err = SessionResult {
            result: false,
            success: true,
            ..Default::default()
        }
        .into_result()
        .unwrap_err();
        assert_eq!(err.api().unwrap().code, "SessionRejected");
    }
}
