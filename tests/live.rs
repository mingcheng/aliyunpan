//! Live integration tests.
//!
//! Credentials come from the repository's `.env` (REFRESH_TOKEN, DEVICE_ID, etc.); rotated tokens are saved there.
//! Tests skip automatically when `.env` is absent. Write tests require `ALIYUNPAN_WRITE_TEST=1`
//! and only use a temporary `/aliyunpan-rs-it-<timestamp>` directory, permanently deleting it afterward.
//!
//! ```fish
//! cargo test --test live -- --nocapture
//! env ALIYUNPAN_WRITE_TEST=1 cargo test --test live -- --nocapture --test-threads=1
//! ```

use std::{
    error::Error as StdError,
    fs,
    path::PathBuf,
    sync::Mutex as StdMutex,
    time::{SystemTime, UNIX_EPOCH},
};

use aliyunpan::{
    CheckNameMode, Client, Config, Credentials, FileItem, ListOptions, Result as SdkResult, TokenStore, UploadOptions,
    UploadSource, UploadStart,
};
use tokio::sync::Mutex;

type TestResult = Result<(), Box<dyn StdError + Send + Sync>>;

/// All cases share one refresh token and must run serially.
static SERIAL: Mutex<()> = Mutex::const_new(());

const KEYS: [&str; 7] = [
    "REFRESH_TOKEN",
    "ACCESS_TOKEN",
    "TOKEN_TYPE",
    "EXPIRES_AT",
    "USER_ID",
    "DEFAULT_DRIVE_ID",
    "DEVICE_ID",
];

macro_rules! ensure {
    ($cond:expr, $($msg:tt)+) => {
        if !$cond {
            return Err(format!($($msg)+).into());
        }
    };
}

/// A `.env`-backed TokenStore that preserves unrelated lines.
struct DotEnvStore {
    path: PathBuf,
    lock: StdMutex<()>,
}

impl DotEnvStore {
    fn new() -> Option<Self> {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".env");
        let store = Self {
            path,
            lock: StdMutex::new(()),
        };
        store.read().get("REFRESH_TOKEN").filter(|v| !v.is_empty())?;
        Some(store)
    }

    fn read(&self) -> std::collections::HashMap<String, String> {
        fs::read_to_string(&self.path)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| l.split_once('='))
            .map(|(k, v)| (k.trim().to_owned(), v.trim().trim_matches('"').to_owned()))
            .collect()
    }
}

impl TokenStore for DotEnvStore {
    fn load(&self) -> SdkResult<Option<Credentials>> {
        let _g = self.lock.lock().unwrap();
        let env = self.read();
        let get = |k: &str| env.get(k).cloned().unwrap_or_default();
        let Some(refresh_token) = env.get("REFRESH_TOKEN").cloned() else {
            return Ok(None);
        };
        Ok(Some(Credentials {
            refresh_token,
            access_token: get("ACCESS_TOKEN"),
            token_type: Some(get("TOKEN_TYPE"))
                .filter(|t| !t.is_empty())
                .unwrap_or_else(|| "Bearer".into()),
            expires_at: get("EXPIRES_AT").parse().unwrap_or(0),
            user_id: get("USER_ID"),
            default_drive_id: get("DEFAULT_DRIVE_ID"),
            device_id: get("DEVICE_ID"),
        }))
    }

    fn save(&self, c: &Credentials) -> SdkResult<()> {
        let _g = self.lock.lock().unwrap();
        let values = [
            c.refresh_token.clone(),
            c.access_token.clone(),
            c.token_type.clone(),
            c.expires_at.to_string(),
            c.user_id.clone(),
            c.default_drive_id.clone(),
            c.device_id.clone(),
        ];
        let old = fs::read_to_string(&self.path).unwrap_or_default();
        let mut out: Vec<String> = old
            .lines()
            .filter(|l| l.split_once('=').is_none_or(|(k, _)| !KEYS.contains(&k.trim())))
            .map(str::to_owned)
            .collect();
        out.extend(KEYS.iter().zip(values).map(|(k, v)| format!("{k}={v}")));
        let tmp = self.path.with_extension("tmp");
        fs::write(&tmp, out.join("\n") + "\n")?;
        fs::rename(&tmp, &self.path)?;
        Ok(())
    }
}

async fn live_client() -> Option<Client> {
    let Some(store) = DotEnvStore::new() else {
        eprintln!("skip: .env with REFRESH_TOKEN not found");
        return None;
    };
    Some(
        Client::connect(Config::default(), store)
            .await
            .expect("connect to alipan"),
    )
}

fn write_enabled() -> bool {
    if std::env::var("ALIYUNPAN_WRITE_TEST").as_deref() == Ok("1") {
        return true;
    }
    eprintln!("skip: set ALIYUNPAN_WRITE_TEST=1 to run write tests");
    false
}

fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()
}

fn random_data(len: usize) -> Vec<u8> {
    let mut buf = vec![0u8; len];
    getrandom::fill(&mut buf).unwrap();
    buf
}

#[tokio::test]
async fn live_read_only() {
    let _serial = SERIAL.lock().await;
    let Some(client) = live_client().await else { return };
    let drive = client.default_drive_id().await;
    assert!(!drive.is_empty());

    let user = client.get_user_info().await.unwrap();
    assert_eq!(user.user_id, client.user_id().await);
    assert_eq!(user.default_drive_id, drive);
    eprintln!("user {} resource_drive={}", user.user_id, user.resource_drive_id);

    let space = client.get_personal_info().await.unwrap().personal_space_info;
    assert!(space.total_size > 0);
    eprintln!("space used={} total={}", space.used_size, space.total_size);

    client.get_sbox_info().await.unwrap();
    let albums = client.get_albums_info().await.unwrap();
    eprintln!("album drive {}", albums.data.drive_id);
    let vip = client.get_vip_info().await.unwrap();
    eprintln!("vip identity {:?}", vip.identity);

    let page = client
        .list_files(
            &drive,
            "root",
            &ListOptions {
                limit: 20,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    eprintln!("root has {} items on first page", page.items.len());
    if let Some(first) = page.items.first() {
        let item = client.get_file(&drive, &first.file_id).await.unwrap();
        assert_eq!(item.file_id, first.file_id);
        let path = client.get_path(&drive, &first.file_id).await.unwrap();
        assert_eq!(path.first().map(|p| p.name.as_str()), Some(first.name.as_str()));
    }

    client
        .list_recycle_bin(
            &drive,
            &ListOptions {
                limit: 10,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    for d in [drive.as_str(), user.resource_drive_id.as_str()]
        .into_iter()
        .filter(|d| !d.is_empty())
    {
        let leftovers = client.list_all_files(d, "root").await.unwrap();
        let trashed = client.list_recycle_bin(d, &ListOptions::default()).await.unwrap().items;
        for item in leftovers
            .iter()
            .chain(&trashed)
            .filter(|i| i.name.starts_with("aliyunpan-rs-it-"))
        {
            eprintln!(
                "WARNING: leftover test folder {} ({}) on drive {d}, trashed={}",
                item.name, item.file_id, item.trashed
            );
        }
    }
    client
        .list_share_links(&ListOptions {
            limit: 10,
            ..Default::default()
        })
        .await
        .unwrap();
    client
        .list_albums(&ListOptions {
            limit: 10,
            order_by: aliyunpan::OrderBy::CreatedAt,
            ..Default::default()
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn live_token_refresh_and_session() {
    let _serial = SERIAL.lock().await;
    let Some(client) = live_client().await else { return };
    let before = client.credentials().await;
    client.force_refresh().await.unwrap();
    let after = client.credentials().await;
    assert_ne!(before.access_token, after.access_token);
    assert_eq!(before.device_id, after.device_id);
    client.recreate_session().await.unwrap();
    client
        .list_files(
            &after.default_drive_id,
            "root",
            &ListOptions {
                limit: 1,
                ..Default::default()
            },
        )
        .await
        .unwrap();
}

async fn purge_sandbox(client: &Client, drive: &str, folder: &FileItem) {
    match client.purge(drive, &[folder.file_id.as_str()]).await {
        Ok(_) => eprintln!("cleaned up {} on {drive}", folder.name),
        Err(e) => eprintln!("WARNING: failed to clean up {} ({}): {e}", folder.name, folder.file_id),
    }
}

#[tokio::test]
async fn live_file_round_trip() {
    let _serial = SERIAL.lock().await;
    if !write_enabled() {
        return;
    }
    let Some(client) = live_client().await else { return };
    let drive = client.default_drive_id().await;
    let sandbox_path = format!("/aliyunpan-rs-it-{}", unix_now());
    let sandbox = client.mkdir_p(&drive, &sandbox_path).await.unwrap();
    ensure_sandbox(&sandbox, &sandbox_path);

    let result = file_round_trip(&client, &drive, &sandbox, &sandbox_path).await;
    purge_sandbox(&client, &drive, &sandbox).await;
    result.unwrap();
}

fn ensure_sandbox(item: &FileItem, path: &str) {
    assert!(
        item.is_folder() && item.file_id != "root",
        "sandbox {path} must be a dedicated folder"
    );
}

async fn file_round_trip(client: &Client, drive: &str, sandbox: &FileItem, sandbox_path: &str) -> TestResult {
    let opts = UploadOptions {
        check_name_mode: CheckNameMode::Refuse,
        chunk_size: Some(512 * 1024),
    };
    let data = random_data(1024 * 1024 + 300 * 1024);

    let first = client
        .upload(drive, &sandbox.file_id, "data.bin", UploadSource::Memory(&data), &opts)
        .await?;
    ensure!(!first.rapid, "random data should not hit rapid upload");
    ensure!(
        first.file.size == data.len() as u64,
        "size mismatch {}",
        first.file.size
    );
    eprintln!("uploaded data.bin ({} bytes, 3 parts)", data.len());

    let second = client
        .upload(
            drive,
            &sandbox.file_id,
            "data-copy.bin",
            UploadSource::Memory(&data),
            &opts,
        )
        .await?;
    ensure!(second.rapid, "second upload of identical content should be rapid");

    let empty = client
        .upload(drive, &sandbox.file_id, "empty.txt", UploadSource::Memory(b""), &opts)
        .await?;
    ensure!(empty.file.size == 0, "empty file size");

    let dup = client
        .upload(drive, &sandbox.file_id, "data.bin", UploadSource::Memory(b"x"), &opts)
        .await;
    ensure!(
        dup.as_ref().is_err_and(|e| e.is_already_exists()),
        "refuse mode should reject duplicate: {dup:?}"
    );

    // Upload part 1 separately, then resume after simulating a restart without cached URLs.
    let resume_data = random_data(700 * 1024);
    let UploadStart::Pending(mut state) = client
        .begin_upload(
            drive,
            &sandbox.file_id,
            "resumed.bin",
            UploadSource::Memory(&resume_data),
            &opts,
        )
        .await?
    else {
        return Err("random data should not be rapid".into());
    };
    ensure!(state.part_count == 2, "expected 2 parts, got {}", state.part_count);
    let urls = client
        .get_upload_url(drive, &state.file_id, &state.upload_id, &[1])
        .await?;
    let (offset, len) = aliyunpan::part_range(1, state.chunk_size, state.local_size);
    let first_part = aliyunpan::Bytes::copy_from_slice(&resume_data[offset as usize..(offset + len) as usize]);
    client
        .upload_part(&urls.part_info_list[0].upload_url, first_part)
        .await?;
    state.completed_parts.insert(1);

    let listed = client
        .list_uploaded_parts(drive, &state.file_id, &state.upload_id, None)
        .await?;
    let numbers: Vec<u32> = listed.uploaded_parts.iter().map(|p| p.part_number).collect();
    eprintln!("list_uploaded_parts: {numbers:?}");
    ensure!(numbers == [1], "server should report part 1 uploaded, got {numbers:?}");

    let mut state: aliyunpan::UploadState = serde_json::from_str(&serde_json::to_string(&state)?)?;
    let mut progressed = Vec::new();
    let resumed = client
        .resume_upload(&mut state, UploadSource::Memory(&resume_data), |s| {
            progressed.push(s.completed_parts.len())
        })
        .await?;
    ensure!(
        progressed == [2],
        "only part 2 should be uploaded on resume, got {progressed:?}"
    );
    ensure!(resumed.size == resume_data.len() as u64, "resumed size");

    let found = client
        .get_by_path(drive, &format!("{sandbox_path}/data.bin"))
        .await?
        .ok_or("data.bin not found")?;
    ensure!(found.file_id == first.file.file_id, "path lookup returned wrong file");

    let full = client.download(drive, &first.file.file_id, None).await?.bytes().await?;
    ensure!(full[..] == data[..], "full download mismatch");
    let range = client.download(drive, &first.file.file_id, Some(100..612)).await?;
    ensure!(range.status() == 206, "range status {}", range.status());
    ensure!(range.bytes().await?[..] == data[100..612], "range download mismatch");

    let tmp_dir = std::env::temp_dir().join(format!("aliyunpan-live-{}", unix_now()));
    fs::create_dir_all(&tmp_dir)?;
    let local = tmp_dir.join("data.bin");
    client.download_to_file(drive, &first.file.file_id, &local).await?;
    ensure!(fs::read(&local)? == data, "download_to_file mismatch");
    let disk = client
        .upload_file(
            drive,
            &sandbox.file_id,
            &local,
            &UploadOptions {
                check_name_mode: CheckNameMode::AutoRename,
                ..opts.clone()
            },
        )
        .await?;
    ensure!(disk.rapid, "re-upload from disk should be rapid");
    fs::remove_dir_all(&tmp_dir)?;

    let deep = client.mkdir_p(drive, &format!("{sandbox_path}/sub/deep")).await?;
    let again = client.mkdir_p(drive, &format!("{sandbox_path}/sub/deep")).await?;
    ensure!(deep.file_id == again.file_id, "mkdir_p should reuse folders");

    for r in client
        .move_files(drive, &[second.file.file_id.as_str()], drive, &deep.file_id)
        .await?
    {
        r.into_result()?;
    }
    let renamed = client
        .rename(drive, &second.file.file_id, "renamed.bin", CheckNameMode::Refuse)
        .await?;
    ensure!(renamed.name == "renamed.bin", "rename failed");
    let moved = client
        .get_by_path(drive, &format!("{sandbox_path}/sub/deep/renamed.bin"))
        .await?;
    ensure!(moved.is_some(), "moved file not found");

    for r in client.set_starred(drive, &[first.file.file_id.as_str()], true).await? {
        r.into_result()?;
    }
    ensure!(
        client.get_file(drive, &first.file.file_id).await?.starred,
        "star failed"
    );
    for r in client.set_starred(drive, &[first.file.file_id.as_str()], false).await? {
        r.into_result()?;
    }

    let mut paths: Vec<String> = client
        .walk(drive, &sandbox.file_id)
        .collect()
        .await?
        .into_iter()
        .map(|e| e.path)
        .collect();
    paths.sort();
    eprintln!("walk: {paths:?}");
    for expected in [
        "data.bin",
        "empty.txt",
        "resumed.bin",
        "sub",
        "sub/deep",
        "sub/deep/renamed.bin",
    ] {
        ensure!(paths.iter().any(|p| p == expected), "walk missing {expected}");
    }

    for r in client.trash(drive, &[first.file.file_id.as_str()]).await? {
        r.into_result()?;
    }
    let bin = client
        .list_recycle_bin(
            drive,
            &ListOptions {
                limit: 100,
                ..Default::default()
            },
        )
        .await?;
    ensure!(
        bin.items.iter().any(|i| i.file_id == first.file.file_id),
        "trashed file not in recycle bin"
    );
    for r in client.restore(drive, &[first.file.file_id.as_str()]).await? {
        r.into_result()?;
    }
    ensure!(
        !client.get_file(drive, &first.file.file_id).await?.trashed,
        "restore failed"
    );

    client.purge(drive, &[empty.file.file_id.as_str()]).await?;
    ensure!(
        client
            .get_file(drive, &empty.file.file_id)
            .await
            .is_err_and(|e| e.is_not_found()),
        "purged file still exists"
    );
    Ok(())
}

#[tokio::test]
async fn live_cross_drive() {
    let _serial = SERIAL.lock().await;
    if !write_enabled() {
        return;
    }
    let Some(client) = live_client().await else { return };
    let drive = client.default_drive_id().await;
    let resource = client.get_user_info().await.unwrap().resource_drive_id;
    if resource.is_empty() || resource == drive {
        eprintln!("skip: no separate resource drive");
        return;
    }
    let sandbox_path = format!("/aliyunpan-rs-it-{}", unix_now());
    let backup_box = client.mkdir_p(&drive, &sandbox_path).await.unwrap();
    ensure_sandbox(&backup_box, &sandbox_path);
    let resource_box = client.mkdir_p(&resource, &sandbox_path).await.unwrap();
    ensure_sandbox(&resource_box, &sandbox_path);

    let result: TestResult = async {
        let up = client
            .upload(
                &drive,
                &backup_box.file_id,
                "cross.bin",
                UploadSource::Memory(&random_data(4096)),
                &UploadOptions::default(),
            )
            .await?;
        let copied = client
            .cross_drive_copy(&drive, &[up.file.file_id.as_str()], &resource, &resource_box.file_id)
            .await?;
        ensure!(copied.len() == 1 && copied[0].is_success(), "copy failed: {copied:?}");
        let moved = client
            .cross_drive_move(&resource, &[copied[0].file_id.as_str()], &drive, &backup_box.file_id)
            .await?;
        ensure!(moved.len() == 1 && moved[0].is_success(), "move failed: {moved:?}");
        let names: Vec<String> = client
            .list_all_files(&drive, &backup_box.file_id)
            .await?
            .into_iter()
            .map(|i| i.name)
            .collect();
        ensure!(names.len() == 2, "expected original + moved copy, got {names:?}");
        Ok(())
    }
    .await;

    purge_sandbox(&client, &resource, &resource_box).await;
    purge_sandbox(&client, &drive, &backup_box).await;
    result.unwrap();
}

#[tokio::test]
async fn live_album_lifecycle() {
    let _serial = SERIAL.lock().await;
    if !write_enabled() {
        return;
    }
    let Some(client) = live_client().await else { return };
    let name = format!("aliyunpan-rs-it-{}", unix_now());
    let album = client.create_album(&name, "integration test").await.unwrap();

    let result: TestResult = async {
        let updated = client
            .update_album(&album.album_id, &format!("{name}-renamed"), "updated")
            .await?;
        ensure!(updated.name.ends_with("-renamed"), "album rename failed");
        let got = client.get_album(&album.album_id).await?;
        ensure!(got.album_id == album.album_id, "get_album mismatch");
        let files = client
            .list_album_files(
                &album.album_id,
                &ListOptions {
                    limit: 10,
                    ..Default::default()
                },
            )
            .await?;
        ensure!(files.items.is_empty(), "new album should be empty");
        Ok(())
    }
    .await;

    client.delete_album(&album.album_id).await.unwrap();
    assert!(client.get_album(&album.album_id).await.is_err(), "album should be gone");
    result.unwrap();
}
