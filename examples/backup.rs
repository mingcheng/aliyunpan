//! Incremental directory backup to the backup (default) drive, and restore.
//!
//! Usage:
//!
//! ```text
//! cargo run --example backup -- <credentials.json> backup  <local_dir> <remote_dir>
//! cargo run --example backup -- <credentials.json> restore <remote_dir> <local_dir>
//! ```
//!
//! `credentials.json` must already be initialized (see the `quickstart` example).
//! Backup skips remote files whose size and SHA1 already match, overwrites changed
//! files, and never deletes remote files. Restore downloads every file under the
//! remote directory and verifies its size and SHA1.

use std::{
    collections::HashMap,
    io::Read,
    path::{Component, Path, PathBuf},
};

use aliyunpan::{CheckNameMode, Client, Config, Error, FileItem, FileStore, Result, UploadOptions};
use sha1::{Digest, Sha1};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [credentials, mode, from, to] = args.as_slice() else {
        eprintln!(
            "usage: backup <credentials.json> (backup <local_dir> <remote_dir> | restore <remote_dir> <local_dir>)"
        );
        std::process::exit(2);
    };
    let client = Client::connect(Config::default(), FileStore::new(credentials)).await?;
    let drive = client.default_drive_id().await;
    match mode.as_str() {
        "backup" => backup(&client, &drive, Path::new(from), to).await,
        "restore" => restore(&client, &drive, from, Path::new(to)).await,
        other => Err(Error::InvalidInput(format!("unknown mode {other:?}"))),
    }
}

async fn backup(client: &Client, drive: &str, local_root: &Path, remote_root: &str) -> Result<()> {
    let root = client.mkdir_p(drive, remote_root).await?;
    let options = UploadOptions {
        check_name_mode: CheckNameMode::Overwrite,
        ..Default::default()
    };
    let (mut uploaded, mut unchanged) = (0, 0);
    let mut pending = vec![(
        local_root.to_path_buf(),
        root.file_id,
        remote_root.trim_end_matches('/').to_owned(),
    )];

    while let Some((dir, folder_id, remote)) = pending.pop() {
        let existing: HashMap<String, FileItem> = client
            .list_all_files(drive, &folder_id)
            .await?
            .into_iter()
            .map(|item| (item.name.clone(), item))
            .collect();

        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let path = entry.path();
            let remote_path = format!("{remote}/{name}");
            let kind = entry.file_type()?;

            if kind.is_dir() {
                let child_id = match existing.get(&name) {
                    Some(item) if item.is_folder() => item.file_id.clone(),
                    _ => {
                        client
                            .create_folder(drive, &folder_id, &name, CheckNameMode::Refuse)
                            .await?
                            .file_id
                    }
                };
                pending.push((path, child_id, remote_path));
            } else if kind.is_file() {
                if let Some(item) = existing.get(&name) {
                    if item.is_file() && item.size == entry.metadata()?.len() && item.sha1_matches(&sha1_hex(&path)?) {
                        unchanged += 1;
                        continue;
                    }
                }
                let outcome = client.upload_file(drive, &folder_id, &path, &options).await?;
                let how = if outcome.rapid { "rapid " } else { "upload" };
                println!("{how} {remote_path}");
                uploaded += 1;
            }
        }
    }
    println!("backup finished: {uploaded} uploaded, {unchanged} unchanged");
    Ok(())
}

async fn restore(client: &Client, drive: &str, remote_root: &str, local_root: &Path) -> Result<()> {
    let folder = match client.get_by_path(drive, remote_root).await? {
        Some(item) if item.is_folder() => item,
        _ => return Err(Error::NotFound(format!("remote folder {remote_root}"))),
    };
    let mut restored = 0;
    let mut walker = client.walk(drive, &folder.file_id);
    while let Some(entry) = walker.next().await {
        let entry = entry?;
        if !entry.item.is_file() {
            continue;
        }
        let dest = safe_join(local_root, &entry.path)?;
        if let Some(parent) = dest.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        client.download_to_file(drive, &entry.item.file_id, &dest).await?;
        println!("restore {}", entry.path);
        restored += 1;
    }
    println!("restore finished: {restored} files");
    Ok(())
}

/// Join a server-provided relative path, rejecting components that could escape `root`.
fn safe_join(root: &Path, relative: &str) -> Result<PathBuf> {
    let relative = Path::new(relative);
    if !relative.components().all(|c| matches!(c, Component::Normal(_))) {
        return Err(Error::InvalidInput(format!("unsafe remote path {relative:?}")));
    }
    Ok(root.join(relative))
}

fn sha1_hex(path: &Path) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha1::new();
    let mut buf = vec![0; 1 << 20];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().iter().map(|b| format!("{b:02X}")).collect())
}
