use std::{
    fmt, fs,
    io::Write,
    path::{Path, PathBuf},
    sync::Mutex,
    time::Duration,
};

use serde::{Deserialize, Serialize};

use crate::{
    error::{Error, Result},
    util,
};

/// Persistent login credentials. Refresh tokens rotate on each refresh; the device ID must remain stable.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Credentials {
    pub refresh_token: String,
    #[serde(default)]
    pub access_token: String,
    #[serde(default = "default_token_type")]
    pub token_type: String,
    /// Access token expiration time in Unix seconds.
    #[serde(default)]
    pub expires_at: i64,
    #[serde(default)]
    pub user_id: String,
    #[serde(default)]
    pub default_drive_id: String,
    #[serde(default)]
    pub device_id: String,
}

fn default_token_type() -> String {
    "Bearer".into()
}

impl Credentials {
    /// Initialize credentials with an externally obtained refresh token and a new device ID.
    pub fn from_refresh_token(refresh_token: impl Into<String>) -> Result<Self> {
        Ok(Self {
            refresh_token: refresh_token.into(),
            access_token: String::new(),
            token_type: default_token_type(),
            expires_at: 0,
            user_id: String::new(),
            default_drive_id: String::new(),
            device_id: util::random_device_id()?,
        })
    }

    pub fn is_expired(&self, margin: Duration) -> bool {
        self.access_token.is_empty()
            || self.user_id.is_empty()
            || util::now_unix().saturating_add(margin.as_secs() as i64) >= self.expires_at
    }

    pub(crate) fn authorization(&self) -> String {
        format!("{} {}", self.token_type, self.access_token)
    }

    pub(crate) fn apply(&mut self, token: TokenResponse) {
        self.access_token = token.access_token;
        if !token.refresh_token.is_empty() {
            self.refresh_token = token.refresh_token;
        }
        if !token.token_type.is_empty() {
            self.token_type = token.token_type;
        }
        self.expires_at = util::now_unix() + token.expires_in;
        self.user_id = token.user_id;
        if !token.default_drive_id.is_empty() {
            self.default_drive_id = token.default_drive_id;
        }
    }
}

impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Credentials")
            .field("refresh_token", &"<redacted>")
            .field("access_token", &"<redacted>")
            .field("token_type", &self.token_type)
            .field("expires_at", &self.expires_at)
            .field("user_id", &self.user_id)
            .field("default_drive_id", &self.default_drive_id)
            .field("device_id", &self.device_id)
            .finish()
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct TokenResponse {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_in: i64,
    pub token_type: String,
    pub user_id: String,
    pub default_drive_id: String,
}

/// Credential storage. `save` is called immediately after token refresh and must persist reliably.
pub trait TokenStore: Send + Sync {
    fn load(&self) -> Result<Option<Credentials>>;
    fn save(&self, credentials: &Credentials) -> Result<()>;
}

/// In-memory credentials. Rotated refresh tokens are lost when the process exits.
#[derive(Debug, Default)]
pub struct MemoryStore(Mutex<Option<Credentials>>);

impl MemoryStore {
    pub fn new(credentials: Credentials) -> Self {
        Self(Mutex::new(Some(credentials)))
    }

    pub fn get(&self) -> Option<Credentials> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

impl TokenStore for MemoryStore {
    fn load(&self) -> Result<Option<Credentials>> {
        Ok(self.get())
    }

    fn save(&self, credentials: &Credentials) -> Result<()> {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = Some(credentials.clone());
        Ok(())
    }
}

/// Atomic JSON file storage with mode 0600 on Unix.
#[derive(Debug, Clone)]
pub struct FileStore {
    path: PathBuf,
}

impl FileStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl TokenStore for FileStore {
    fn load(&self) -> Result<Option<Credentials>> {
        match fs::read(&self.path) {
            Ok(data) => crate::error::decode(&data).map(Some),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    fn save(&self, credentials: &Credentials) -> Result<()> {
        let data = serde_json::to_vec_pretty(credentials)
            .map_err(|e| Error::InvalidInput(format!("serialize credentials: {e}")))?;
        let mut tmp = self.path.clone().into_os_string();
        tmp.push(format!(".{}.tmp", util::random_device_id()?));
        let tmp = PathBuf::from(tmp);

        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let mut file = options.open(&tmp)?;
        let result = (|| {
            file.write_all(&data)?;
            file.sync_all()?;
            drop(file);
            fs::rename(&tmp, &self.path)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        result.map_err(Error::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expiry_check() {
        let mut c = Credentials::from_refresh_token("rt").unwrap();
        assert!(c.is_expired(Duration::ZERO));
        c.apply(TokenResponse {
            access_token: "at".into(),
            refresh_token: "rt2".into(),
            expires_in: 7200,
            token_type: "Bearer".into(),
            user_id: "u".into(),
            default_drive_id: "d".into(),
        });
        assert_eq!(c.refresh_token, "rt2");
        assert_eq!(c.authorization(), "Bearer at");
        assert!(!c.is_expired(Duration::from_secs(60)));
        assert!(c.is_expired(Duration::from_secs(7200)));
    }

    #[test]
    fn apply_keeps_old_refresh_token_when_absent() {
        let mut c = Credentials::from_refresh_token("rt").unwrap();
        c.apply(TokenResponse {
            access_token: "at".into(),
            user_id: "u".into(),
            ..Default::default()
        });
        assert_eq!(c.refresh_token, "rt");
        assert_eq!(c.token_type, "Bearer");
    }

    #[test]
    fn debug_redacts_tokens() {
        let c = Credentials::from_refresh_token("super-secret").unwrap();
        assert!(!format!("{c:?}").contains("super-secret"));
    }

    #[test]
    fn file_store_round_trip() {
        let dir = std::env::temp_dir().join(format!("aliyunpan-store-{}", util::random_device_id().unwrap()));
        fs::create_dir_all(&dir).unwrap();
        let store = FileStore::new(dir.join("creds.json"));
        assert!(store.load().unwrap().is_none());
        let existing_tmp = dir.join("creds.json.tmp");
        fs::write(&existing_tmp, b"unrelated data").unwrap();

        let creds = Credentials::from_refresh_token("rt").unwrap();
        store.save(&creds).unwrap();
        assert_eq!(store.load().unwrap(), Some(creds.clone()));
        assert_eq!(fs::read(&existing_tmp).unwrap(), b"unrelated data");
        std::thread::scope(|scope| {
            for _ in 0..8 {
                scope.spawn(|| store.save(&creds).unwrap());
            }
        });
        assert_eq!(store.load().unwrap(), Some(creds));
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 2);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(store.path()).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn minimal_json_is_accepted() {
        let c: Credentials = serde_json::from_str(r#"{"refresh_token":"rt"}"#).unwrap();
        assert_eq!(c.token_type, "Bearer");
        assert!(c.device_id.is_empty());
    }
}
