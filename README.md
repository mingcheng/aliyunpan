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

The default TLS backend is `rustls`. To use the operating system's TLS implementation:

```toml
aliyunpan = { version = "1.0", default-features = false, features = ["native-tls"] }
```

On Linux, `native-tls` requires OpenSSL development headers and `pkg-config`
(`libssl-dev` on Ubuntu, `libopenssl-devel` on openSUSE). Enable at least one TLS
backend when connecting to the production HTTPS endpoints.

The library has nine direct dependencies: `reqwest`, `tokio`, `serde`, `serde_json`,
`bytes`, `k256`, `sha1`, `md-5`, and `getrandom`. Server timestamps remain RFC3339
strings instead of being converted to a date/time library's types.

## Quick Start

Obtain a refresh token outside this SDK. The SDK does not implement an interactive
login flow. Initialize the credential store once; do not overwrite rotated tokens
on subsequent runs.

```no_run
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

The example program lists account usage and root files:

```sh
cargo run --example quickstart -- credentials.json
```

It also accepts an initial refresh token as a second argument, but command-line
arguments may appear in process listings or shell history. Prefer a credential
file initialized by your application. Never commit tokens, credential files, or
presigned transfer URLs.

## Credentials and Concurrency

Use one `Client` per credential set and clone it to share it across tasks. Clones
share token and session state. Do not refresh the same credentials from multiple
independent clients or processes: refresh tokens rotate on every successful refresh.

`FileStore` atomically replaces its JSON file using an exclusively created temporary
file, with mode `0600` on Unix. `MemoryStore` loses rotated credentials when the
process exits. Custom `TokenStore` implementations must persist credentials reliably.

If persistence fails after a successful refresh, an existing client retains the
new credentials, returns the storage error, and retries saving before subsequent
requests. Resolve this failure before exiting; otherwise the new token may be lost.
An initial `Client::connect` failure does not return a usable client, so ensure the
store is writable before connecting. A stable `device_id` is part of the credentials.

## Transfers and Recovery

`upload_file` handles a complete upload. `begin_upload` returns either
`UploadStart::Rapid` when no data transfer is needed, or `UploadStart::Pending`
with a serializable `UploadState`.

For restart recovery, save the pending state before transferring data and persist
updates from the `resume_upload` progress callback. Presigned URLs are deliberately
excluded from serialized state and are fetched again when needed. The callback is
synchronous and cannot return an error; callers must handle checkpoint failures
explicitly. `list_all_uploaded_parts` can help reconcile server-side progress.

```no_run
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

Resume verifies both source size and SHA1 before transferring remaining parts.
This requires a full read of the source; do not modify it during hashing or upload.
The default chunk size is 512 KiB and grows automatically to stay within 10,000
parts. Backup jobs should choose `Refuse` or `Overwrite` deliberately.

`download` returns a stream and accepts half-open byte ranges. `download_to_file`
writes to `<destination>.part`, verifies the size and available SHA1, and renames
the file only on success. Do not concurrently download to the same destination.

## Configuration and Errors

`Config` controls endpoints, chunk size, retry count and delay, JSON request timeout,
transfer read timeout, token refresh margin, and pagination delay. Defaults target
the web API; the legacy download Referer is intentional.

`Error` distinguishes transport, API, persistence, decoding, and integrity failures.
Use `api_kind()`, `needs_relogin()`, `is_not_found()`, and `is_already_exists()` to
classify errors without matching human-readable messages. Batch calls retain each
sub-response: an outer `Ok` does not guarantee every operation succeeded.
Avoid blindly retrying mutations after an ambiguous network failure, and treat
error bodies and debug output as potentially sensitive account data.

## API Guide

Public types and client methods are re-exported at the crate root. Generate the
reference documentation with `cargo doc --no-deps --open`.

| Area | Entry points |
| --- | --- |
| Authentication | `Client`, `Credentials`, `TokenStore`, `FileStore`, `MemoryStore` |
| Files and paths | `list_files`, `list_all_files`, `get_file`, `mkdir_p`, `Walker` |
| Uploads | `UploadSource`, `UploadOptions`, `UploadState`, `begin_upload`, `resume_upload` |
| Downloads | `DownloadStream`, `download`, `download_to_file` |
| Batch operations | `BatchRequest`, `BatchResponse`, `BatchVersion` |
| Response data | `FileItem` and the other model types exported from the crate root |

## Development and Tests

Run the checks that do not require an account:

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --lib --test mock
cargo test --locked --doc
cargo +1.85.0 test --locked --lib --test mock
cargo doc --locked --no-deps
```

With the OpenSSL development dependencies installed, also run:

```sh
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --no-default-features --features native-tls --lib --test mock
```

Unit and mock tests use local data and a loopback HTTP server. Cargo may still need
network access to download dependencies. The README examples are compiled as
Rustdoc tests without executing their account operations.

Live tests read an ignored `.env` file containing at least `REFRESH_TOKEN` and
`DEVICE_ID`. Even read-only tests refresh tokens and rewrite that file. Do not run
them while another client is using the same credentials.

```sh
cargo test --test live -- --nocapture --test-threads=1
env ALIYUNPAN_WRITE_TEST=1 cargo test --test live -- --nocapture --test-threads=1
```

Write tests operate in a temporary `/aliyunpan-rs-it-<timestamp>` folder and a
similarly named album, then permanently delete their test resources. Inspect and
remove leftover test resources if a test is interrupted. Never enable write tests
against an account without understanding these operations.

## CI and Releases

CI checks formatting, Clippy, documentation, security advisories, and package
builds. Tests run on Rust 1.85, stable, beta, and nightly; nightly failures are
non-blocking. Stable also tests the `native-tls` backend. CI does not use real
account credentials or run live tests.

1. Update the package version and lockfile, then run the checks above.
2. Verify a clean working tree with `cargo package --locked`.
3. Create and push a `v<version>` tag matching the package version exactly.

Only matching version tags can publish. Manual runs must also select a version
tag and default to a dry run. Actual publication requires the repository secret
`CARGO_REGISTRY_TOKEN`. Branch pushes and pull requests never publish.
Publishing is serialized and is not canceled by a newer release run.

The package uses a file allowlist that excludes local credentials and editor
configuration. Review `cargo package --list` before changing that allowlist.

## License

MIT. See LICENSE for the full text.
