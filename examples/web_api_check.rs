//! API monitor with immediate GitHub Environment Secret persistence.
//! Usage: web_api_check <credentials.json> [--persist-only | --full <fixture-directory>]

#[path = "web_api_check/suite.rs"]
mod suite;

use std::{
    fs::File,
    path::PathBuf,
    process::{Command, ExitCode, Stdio},
    time::Duration,
};

use aliyunpan::{Client, Config, Credentials, Error, FileStore, ListOptions, Result, TokenStore};

const ENVIRONMENT: &str = "aliyunpan-monitor";
const SECRET: &str = "ALIYUNPAN_CREDENTIALS";

struct GitHubStore {
    file: FileStore,
    repository: String,
    gh: PathBuf,
}

impl GitHubStore {
    fn publish(&self) -> Result<()> {
        // Retrying this idempotent write must never refresh the Aliyun token again.
        for attempt in 0..3 {
            let status = Command::new(&self.gh)
                .args([
                    "secret",
                    "set",
                    SECRET,
                    "--env",
                    ENVIRONMENT,
                    "--repo",
                    &self.repository,
                ])
                .stdin(File::open(self.file.path())?)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()?;
            if status.success() {
                return Ok(());
            }
            eprintln!(
                "GitHub credential write failed (attempt {}/3); check writer permissions and connectivity",
                attempt + 1
            );
            if attempt < 2 {
                std::thread::sleep(Duration::from_secs(2));
            }
        }
        Err(std::io::Error::other("GitHub credential persistence failed").into())
    }
}

impl TokenStore for GitHubStore {
    fn load(&self) -> Result<Option<Credentials>> {
        let mut credentials = self.file.load()?;
        if let Some(c) = &mut credentials {
            // Exercise refresh even when a manual rerun happens before token expiry.
            c.access_token.clear();
            c.expires_at = 0;
        }
        Ok(credentials)
    }

    fn save(&self, credentials: &Credentials) -> Result<()> {
        self.file.save(credentials)?;
        self.publish()
    }
}

fn checked<T>(stage: &str, result: Result<T>) -> std::result::Result<T, String> {
    result.map_err(|error| {
        // Raw API/HTTP/decode errors can contain credentials or private account data.
        let detail = match error {
            Error::Api(e) => format!("API {:?}, HTTP {}", e.kind(), e.http_status),
            Error::Http { status, .. } | Error::RateLimited { status, .. } => format!("HTTP {status}"),
            Error::Network(_) => "network failure".into(),
            Error::Decode { .. } => "unexpected response schema".into(),
            Error::UnexpectedResponse { operation, shape } => format!("{operation}: {shape}"),
            Error::Io(e) => format!("credential storage/process failure ({:?})", e.kind()),
            Error::NotLoggedIn => "missing refresh token".into(),
            Error::InvalidInput(_) => "invalid input or incomplete response".into(),
            _ => "SDK failure".into(),
        };
        format!("{stage}: {detail}")
    })
}

fn initial_credentials(json: &str) -> std::result::Result<Credentials, String> {
    let mut credentials: Credentials =
        serde_json::from_str(json).map_err(|_| "ALIYUNPAN_CREDENTIALS must be valid credential JSON")?;
    if credentials.refresh_token.trim().is_empty() {
        return Err("ALIYUNPAN_CREDENTIALS requires a nonempty refresh_token".into());
    }
    if credentials.device_id.trim().is_empty() {
        credentials.device_id = checked(
            "create device ID",
            Credentials::from_refresh_token(&credentials.refresh_token),
        )?
        .device_id;
    }
    Ok(credentials)
}

async fn probe(config: Config, store: GitHubStore) -> std::result::Result<(), String> {
    let client = checked(
        "refresh token / persist credentials / create device session",
        Client::connect(config, store).await,
    )?;
    println!("OK: token refresh, credential persistence, device session");

    let drive = client.default_drive_id().await;
    let user = checked("user info", client.get_user_info().await)?;
    if drive.is_empty() || user.user_id != client.user_id().await || user.default_drive_id != drive {
        return Err("user info: missing or inconsistent user/drive identifiers".into());
    }
    println!("OK: user info");

    let space = checked("personal space", client.get_personal_info().await)?.personal_space_info;
    if space.total_size == 0 {
        return Err("personal space: missing or zero total_size".into());
    }
    println!("OK: personal space");

    let page = checked(
        "root file listing",
        client
            .list_files(
                &drive,
                "root",
                &ListOptions {
                    limit: 1,
                    ..Default::default()
                },
            )
            .await,
    )?;
    if page.items.iter().any(|item| item.file_id.is_empty()) {
        return Err("root file listing: missing file identifier".into());
    }
    println!("OK: root file listing");
    Ok(())
}

fn required_env(name: &str) -> std::result::Result<String, String> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| format!("missing required environment variable: {name}"))
}

async fn run() -> std::result::Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let full = args.len() == 3 && args[1] == "--full";
    let persist_only = args.len() == 2 && args[1] == "--persist-only";
    if args.len() != 1 && !full && !persist_only {
        return Err("usage: web_api_check <credentials.json> [--persist-only | --full <fixture-directory>]".into());
    }
    required_env("GH_TOKEN")?;
    let store = GitHubStore {
        file: FileStore::new(&args[0]),
        repository: required_env("GH_REPO")?,
        gh: "gh".into(),
    };
    if persist_only {
        let credentials = checked("load checkpoint", store.file.load())?
            .ok_or_else(|| "credential checkpoint not found".to_owned())?;
        if credentials.refresh_token.trim().is_empty() || credentials.device_id.trim().is_empty() {
            return Err("credential checkpoint lacks refresh_token or device_id".into());
        }
        return checked("recover GitHub credential persistence", store.publish());
    }

    if full {
        for file in ["fixture.jpg", "fixture.mp4"] {
            let path = std::path::Path::new(&args[2]).join(file);
            if !path.is_file() {
                return Err(format!(
                    "full API tests require a generated {file} in the fixture directory"
                ));
            }
        }
    }
    let credentials = initial_credentials(&required_env(SECRET)?)?;
    // Verify write access before consuming a rotating token, and persist a new device ID.
    checked("credential persistence preflight", store.save(&credentials))?;
    let config = Config {
        max_retries: 2,
        request_timeout: Duration::from_secs(30),
        ..Config::default()
    };
    if full {
        suite::run(config, store, std::path::Path::new(&args[2])).await
    } else {
        probe(config, store).await
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(all(test, unix))]
#[path = "../tests/common/mod.rs"]
mod common;

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use common::{Auth, DRIVE_ID, MockServer, Response, USER_ID};
    use serde_json::json;
    use std::{fs, os::unix::fs::PermissionsExt, sync::Arc};

    struct Fixture {
        dir: PathBuf,
    }

    impl Fixture {
        fn new(script: &str) -> Self {
            let id = Credentials::from_refresh_token("test").unwrap().device_id;
            let dir = std::env::temp_dir().join(format!("aliyunpan-monitor-{id}"));
            fs::create_dir(&dir).unwrap();
            let gh = dir.join("gh");
            fs::write(&gh, format!("#!/bin/sh\nset -eu\n{script}\n")).unwrap();
            fs::set_permissions(&gh, fs::Permissions::from_mode(0o700)).unwrap();
            Self { dir }
        }

        fn store(&self) -> GitHubStore {
            GitHubStore {
                file: FileStore::new(self.dir.join("credentials.json")),
                repository: "owner/repo".into(),
                gh: self.dir.join("gh"),
            }
        }

        fn published(&self) -> Credentials {
            serde_json::from_slice(&fs::read(self.dir.join("published.json")).unwrap()).unwrap()
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.dir).unwrap();
        }
    }

    const PUBLISH: &str = r#"
test "$*" = "secret set ALIYUNPAN_CREDENTIALS --env aliyunpan-monitor --repo owner/repo"
cat > "$(dirname "$0")/published.json"
"#;

    #[test]
    fn validates_initial_credentials_and_preserves_device() {
        for value in ["", "{}", r#"{"refresh_token":" "}"#, "not-json"] {
            assert!(initial_credentials(value).is_err());
        }
        let c = initial_credentials(r#"{"refresh_token":"rt"}"#).unwrap();
        assert!(!c.device_id.is_empty());
        let json = serde_json::to_string(&c).unwrap();
        assert_eq!(initial_credentials(&json).unwrap(), c);
    }

    #[test]
    fn store_persists_rotation_and_forces_next_refresh() {
        let fixture = Fixture::new(PUBLISH);
        let store = fixture.store();
        let mut credentials = common::initial_credentials();
        credentials.access_token = "still-valid".into();
        credentials.expires_at = i64::MAX;
        store.save(&credentials).unwrap();
        assert_eq!(fixture.published(), credentials);
        let loaded = store.load().unwrap().unwrap();
        assert!(loaded.access_token.is_empty());
        assert_eq!(loaded.expires_at, 0);
        assert_eq!(loaded.device_id, credentials.device_id);
        assert_eq!(loaded.refresh_token, credentials.refresh_token);
    }

    #[test]
    fn failed_publish_keeps_checkpoint_for_recovery() {
        let fixture = Fixture::new("exit 1");
        let store = fixture.store();
        let credentials = common::initial_credentials();
        assert!(store.save(&credentials).is_err());
        assert_eq!(store.file.load().unwrap(), Some(credentials.clone()));
        fs::write(&store.gh, format!("#!/bin/sh\nset -eu\n{PUBLISH}\n")).unwrap();
        store.publish().unwrap();
        assert_eq!(fixture.published(), credentials);
    }

    #[test]
    fn transient_publish_failure_is_retried() {
        let fixture = Fixture::new(&format!(
            r#"
if test ! -f "$(dirname "$0")/attempted"; then
    touch "$(dirname "$0")/attempted"
    exit 1
fi
{PUBLISH}
"#
        ));
        let credentials = common::initial_credentials();
        fixture.store().save(&credentials).unwrap();
        assert_eq!(fixture.published(), credentials);
    }

    #[tokio::test]
    async fn failed_rotation_publish_stops_requests_and_can_be_recovered() {
        let fixture = Fixture::new(PUBLISH);
        let store = fixture.store();
        store.save(&common::initial_credentials()).unwrap();
        fs::write(&store.gh, "#!/bin/sh\nexit 1\n").unwrap();
        let auth = Auth::default();
        let server = MockServer::start(move |req| {
            auth.handle(req)
                .unwrap_or_else(|| Response::error(404, "UnexpectedRequest"))
        })
        .await;
        let error = probe(common::config(&server.url), store).await.unwrap_err();
        assert!(error.contains("credential storage/process failure"));
        assert_eq!(server.count("/v2/account/token"), 1);
        assert_eq!(server.count("/users/v1/users/device/create_session"), 0);
        assert_eq!(fixture.published().refresh_token, "rt-0");
        let store = fixture.store();
        assert_eq!(store.file.load().unwrap().unwrap().refresh_token, "rt-1");

        fs::write(&store.gh, format!("#!/bin/sh\nset -eu\n{PUBLISH}\n")).unwrap();
        store.publish().unwrap();
        assert_eq!(fixture.published().refresh_token, "rt-1");
        assert_eq!(server.count("/v2/account/token"), 1);
    }

    async fn mock_probe(fail_session: bool, fail_user: bool, malformed_user: bool) {
        let fixture = Fixture::new(PUBLISH);
        let store = fixture.store();
        store.save(&common::initial_credentials()).unwrap();
        let auth = Arc::new(Auth::default());
        let published = fixture.dir.join("published.json");
        let server = MockServer::start(move |req| {
            if req.path == "/users/v1/users/device/create_session" {
                let saved: Credentials = serde_json::from_slice(&fs::read(&published).unwrap()).unwrap();
                assert_eq!(saved.refresh_token, "rt-1");
                if fail_session {
                    return Response::error(403, "DeviceRejected");
                }
            }
            if let Some(response) = auth.handle(req) {
                return response;
            }
            assert_eq!(req.header("authorization"), Some(auth.token().as_str()));
            if req.path != "/v2/user/get" && req.path != "/v2/databox/get_personal_info" {
                if let Some(response) = auth.check(req) {
                    return response;
                }
            }
            match req.path.as_str() {
                "/v2/user/get" if fail_user => Response::error(403, "PermissionDenied"),
                "/v2/user/get" if malformed_user => Response::json(json!({})),
                "/v2/user/get" => Response::json(json!({
                    "user_id": USER_ID, "default_drive_id": DRIVE_ID
                })),
                "/v2/databox/get_personal_info" => {
                    Response::json(json!({"personal_space_info": {"total_size": 1024, "used_size": 0}}))
                }
                "/adrive/v3/file/list" => Response::json(json!({"items": [], "next_marker": ""})),
                _ => Response::error(404, "UnexpectedRequest"),
            }
        })
        .await;
        let result = probe(common::config(&server.url), store).await;
        assert_eq!(result.is_err(), fail_session || fail_user || malformed_user);
        assert_eq!(fixture.published().refresh_token, "rt-1");
        assert_eq!(fixture.published().device_id, common::DEVICE_ID);
        assert_eq!(server.count("/v2/account/token"), 1);
        if result.is_ok() {
            assert_eq!(server.count("/adrive/v3/file/list"), 1);
        }
    }

    #[tokio::test]
    async fn read_only_probe_accepts_empty_drive() {
        mock_probe(false, false, false).await;
    }

    #[tokio::test]
    async fn rotation_is_saved_before_session_failure() {
        mock_probe(true, false, false).await;
    }

    #[tokio::test]
    async fn rotation_is_saved_before_endpoint_failure() {
        mock_probe(false, true, false).await;
    }

    #[tokio::test]
    async fn incomplete_user_response_fails() {
        mock_probe(false, false, true).await;
    }

    #[test]
    fn diagnostics_do_not_disclose_response_bodies() {
        let message = checked::<()>(
            "test",
            Err(Error::Http {
                status: 400,
                body: "private-token".into(),
            }),
        )
        .unwrap_err();
        assert_eq!(message, "test: HTTP 400");
    }
}
