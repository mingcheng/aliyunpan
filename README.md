# aliyunpan

An unofficial asynchronous Rust SDK for the Aliyun Drive (alipan.com) web API.

This is **not an official** Open Platform client. Web endpoints can change without notice,
and some operations are restricted by account, drive type, or server policy.
Use an account and files you are authorized to access.

## Features

- Automatic token refresh and persistence of rotated refresh tokens.
- Signed secp256k1 device sessions and concurrency-safe authentication recovery.
- Bounded retries for rate limits and transient server failures.
- File metadata, pagination, path lookup, recursive directory creation and traversal.
- SHA1-based rapid uploads, multipart uploads, URL renewal, and resumable upload state.
- Streaming and ranged downloads, expiring URL recovery, and verified file downloads.
- Recycle bin, batch operations, cross-drive transfers, shares, quick transfers, albums,
  and video previews. See method documentation for restrictions and unverified endpoints.

## Installation

```toml
[dependencies]
aliyunpan = "1.0"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

The default TLS backend is `rustls`. To use the operating system's TLS implementation
(on Linux this needs OpenSSL headers and `pkg-config`):

```toml
aliyunpan = { version = "1.0", default-features = false, features = ["native-tls"] }
```

## Quick Start

Obtain a refresh token outside this SDK; there is no interactive login flow.
Initialize the credential store once and never overwrite rotated tokens afterwards.

```rust
use aliyunpan::{Client, Config, Credentials, FileStore, TokenStore, UploadOptions};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
	let store = FileStore::new("credentials.json");
	if store.load()?.is_none() {
		let token = std::env::var("ALIYUNPAN_REFRESH_TOKEN")?;
		store.save(&Credentials::from_refresh_token(token)?)?;
	}
	let client = Client::connect(Config::default(), store).await?;

	let drive = client.default_drive_id().await;
	let directory = client.mkdir_p(&drive, "/backup/2026").await?;
	let uploaded = client
		.upload_file(&drive, &directory.file_id, "data.bin".as_ref(), &UploadOptions::default())
		.await?;
	client
		.download_to_file(&drive, &uploaded.file.file_id, "restore.bin".as_ref())
		.await?;
	Ok(())
}
```

```sh
cargo run --example quickstart -- credentials.json   # account usage and root listing
```

Never commit tokens, credential files, or presigned transfer URLs.

## Backing Up Files

`default_drive_id()` is the **backup drive**. `UserInfo::resource_drive_id` (from
`get_user_info`) is the separate resource drive, when the account has one.

### Back up a file only when it changed

`UploadOptions` defaults to `CheckNameMode::Refuse`, which reports an existing
name instead of replacing it. Backup jobs usually want `Overwrite` after checking
whether the remote copy is already current:

```rust
use aliyunpan::{CheckNameMode, Client, UploadOptions};
use std::path::Path;

async fn backup_file(client: &Client, local: &Path, remote_dir: &str) -> aliyunpan::Result<()> {
	let drive = client.default_drive_id().await;
	let folder = client.mkdir_p(&drive, remote_dir).await?;
	let name = local.file_name().and_then(|n| n.to_str()).unwrap_or_default();

	if let Some(existing) = client.find_child(&drive, &folder.file_id, name).await? {
		// For a stricter check, also compare `existing.sha1_matches(local_sha1)`.
		if existing.is_file() && existing.size == std::fs::metadata(local)?.len() {
			return Ok(());
		}
	}
	let options = UploadOptions { check_name_mode: CheckNameMode::Overwrite, ..Default::default() };
	let outcome = client.upload_file(&drive, &folder.file_id, local, &options).await?;
	println!("{} uploaded (rapid: {})", outcome.file.name, outcome.rapid);
	Ok(())
}
```

When the server already has identical content, the upload completes as a rapid
upload without transferring data. Large uploads can be checkpointed and resumed
(see [Transfers and Recovery](#transfers-and-recovery)).

### Restore a backup directory

```rust
use aliyunpan::Client;
use std::path::Path;

async fn restore(client: &Client, remote_dir: &str, local_root: &Path) -> aliyunpan::Result<()> {
	let drive = client.default_drive_id().await;
	let Some(folder) = client.get_by_path(&drive, remote_dir).await? else {
		return Ok(());
	};
	let mut walker = client.walk(&drive, &folder.file_id);
	while let Some(entry) = walker.next().await {
		let entry = entry?;
		if entry.item.is_file() {
			// Validate `entry.path` before trusting it as a local path.
			let dest = local_root.join(&entry.path);
			tokio::fs::create_dir_all(dest.parent().unwrap()).await?;
			client.download_to_file(&drive, &entry.item.file_id, &dest).await?;
		}
	}
	Ok(())
}
```

`download_to_file` writes `<destination>.part`, verifies the size and SHA1, and
renames the file only on success.

### Move files from the resource drive into the backup drive

```rust
use aliyunpan::Client;

async fn archive(client: &Client, resource_file_ids: &[&str]) -> aliyunpan::Result<()> {
	let resource = client.get_user_info().await?.resource_drive_id;
	let backup = client.default_drive_id().await;
	let folder = client.mkdir_p(&backup, "/backup/archive").await?;
	for item in client.cross_drive_move(&resource, resource_file_ids, &backup, &folder.file_id).await? {
		if !item.is_success() {
			eprintln!("failed to move {} (status {})", item.source_file_id, item.status);
		}
	}
	Ok(())
}
```

`cross_drive_move` only supports resource-to-backup moves. Use `cross_drive_copy`
to copy in either direction. Check every returned item: an outer `Ok` does not
mean every file was transferred.

### Complete example

[examples/backup.rs](examples/backup.rs) incrementally backs up a local directory
(skipping files whose size and SHA1 already match) and restores it:

```sh
cargo run --example backup -- credentials.json backup  ./photos /backup/photos
cargo run --example backup -- credentials.json restore /backup/photos ./restored
```

It never deletes remote files; files removed locally stay in the backup.

## Credentials and Concurrency

- Use one `Client` per credential set and clone it to share it across tasks.
  Refresh tokens rotate on every refresh, so never refresh the same credentials
  from multiple independent clients or processes.
- `FileStore` replaces its JSON file atomically (mode `0600` on Unix).
  `MemoryStore` loses rotated credentials when the process exits.
- If persisting a refreshed token fails, the client keeps the new credentials,
  returns the storage error, and retries saving before the next request. Resolve
  the failure before exiting, or the new token may be lost.
- A stable `device_id` is part of the credentials.

## Transfers and Recovery

`upload_file` handles a complete upload. `begin_upload` returns `UploadStart::Rapid`
when no data transfer is needed, or `UploadStart::Pending` with a serializable
`UploadState`. Save that state before transferring data and persist updates from
the `resume_upload` progress callback. Presigned URLs are never serialized.

```rust
use aliyunpan::{Client, FileItem, UploadSource, UploadState};
use std::path::Path;

async fn resume(
	client: &Client,
	source: &Path,
	saved_state: &str,
) -> Result<FileItem, Box<dyn std::error::Error>> {
	let mut state: UploadState = serde_json::from_str(saved_state)?;
	let file = client
		.resume_upload(&mut state, UploadSource::File(source), |progress| {
			println!("{} bytes uploaded", progress.uploaded_bytes());
		})
		.await?;
	Ok(file)
}
```

Resume re-verifies the source size and SHA1, so do not modify the source during
an upload. The default chunk size is 512 KiB and grows to stay within 10,000 parts.
`download` returns a stream and accepts half-open byte ranges. Do not download
concurrently to the same destination.

## Configuration and Errors

`Config` controls endpoints, chunk size, retries, timeouts, the token refresh
margin, and pagination delay.

`Error` distinguishes transport, API, persistence, decoding, and integrity failures.
Use `api_kind()`, `needs_relogin()`, `is_not_found()`, and `is_already_exists()`
instead of matching messages. Batch and cross-drive calls return per-item results.
Do not blindly retry mutations after an ambiguous network failure; JSON mutations
are never replayed automatically. Treat error bodies as potentially sensitive.

## API Guide

Public types and client methods are re-exported at the crate root
(`cargo doc --no-deps --open`).

| Area             | Entry points                                                                                         |
| ---------------- | ---------------------------------------------------------------------------------------------------- |
| Authentication   | `Client`, `Credentials`, `TokenStore`, `FileStore`, `MemoryStore`                                    |
| Files and paths  | `list_files`, `list_all_files`, `get_file`, `get_by_path`, `find_child`, `mkdir_p`, `walk`, `rename` |
| Uploads          | `UploadSource`, `UploadOptions`, `UploadState`, `upload_file`, `begin_upload`, `resume_upload`       |
| Downloads        | `DownloadStream`, `download`, `download_to_file`                                                     |
| Cross-drive      | `cross_drive_copy`, `cross_drive_move`                                                               |
| Batch operations | `BatchRequest`, `BatchResponse`, `BatchVersion`                                                      |
| Response data    | `FileItem` and the other model types exported from the crate root                                    |

### Known limitations

- `renew_session` is currently rejected by the server with
  `DeviceSessionSignatureInvalid`; the client recovers by recreating the session.
- Share, album-share and quick-transfer creation may be refused depending on the account.

## Development

Checks that do not need an account (unit tests and a loopback mock server):

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --lib --test mock --example web_api_check
cargo test --locked --doc
cargo doc --locked --no-deps
```

Live tests read `REFRESH_TOKEN` and `DEVICE_ID` from an ignored `.env` file and
rewrite it with rotated tokens. Write tests use a temporary
`/aliyunpan-rs-it-<timestamp>` folder and delete it afterwards. Do not run them
while another client uses the same credentials.

```sh
cargo test --test live -- --nocapture --test-threads=1
env ALIYUNPAN_WRITE_TEST=1 cargo test --test live -- --nocapture --test-threads=1
```

## License

This project is licensed under the MIT License. See [LICENSE](LICENSE) for the full text.
