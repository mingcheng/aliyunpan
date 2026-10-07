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
cargo test --locked --example web_api_check
cargo test --locked --example web_api_check
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
non-blocking. Stable also tests the `native-tls` backend. Project push/PR CI does
not use real account credentials or run live tests. Live API contract tests run
in the independent workflow described below; they do not gate project CI.

### Daily live API contract tests

[Web API Contract Tests](.github/workflows/web-api-check.yml) runs daily at
**03:17 UTC / 11:17 Asia/Shanghai**, and supports manual dispatch on the default
branch. This is a **read/write integration suite**, not just a health check.
It runs the [monitor tests](examples/web_api_check/suite_tests.rs) without credentials
first, generates a synthetic JPEG and a two-second MP4 using FFmpeg, then runs
the [live suite](examples/web_api_check/suite.rs) with `--full`.

The suite inventories all public SDK HTTP operations, plus their high-level
helpers. An inventory test fails when a new public async API is added without
being classified. Pure local helpers and credential/stream accessors are not
separate endpoint tests. Operations sharing an endpoint still have distinct
contract cases; `connect_shared` is exercised by `connect`, and upload completion
is also exercised by the multipart resume test.

| Area | Live operations and assertions |
| --- | --- |
| Authentication / account | Refresh and persist credentials, preserve device ID, create/recreate/renew session; user identity, space, insurance-box info, album drive, VIP info |
| Files / paths | Create a unique folder, get metadata and ancestry, child/absolute-path lookup, recursive mkdir, page-marker traversal, list-all, recursive walk |
| Upload | Random multipart upload, low-level create/URL/part/complete operations, list received parts, serialized checkpoint resume, empty file, rapid upload from disk |
| Download | URL retrieval, streamed download, exact ranged bytes and HTTP 206, disk download; compare sizes, content and SHA1 |
| Mutations / batch | Check every subresponse and expected count, move, rename, star/unstar and aliases; confirm resulting metadata |
| Recycle bin | Trash a generated test file, list/list-all, restore, permanently delete that file, purge only the owned test directories; confirm disappearance |
| Cross-drive | Copy backup-to-resource and move resource-to-backup; verify destination, content hash and source disappearance |
| Sharing | Password-protected one-day share of synthetic text, anonymous metadata, token and URL-token helpers, list/list-all shared files, save into a test directory, cancel the test share |
| Quick transfer | Both quick-transfer entry points, using synthetic text only; these links cannot be canceled through the SDK and expire after about 24 hours |
| Albums | Create/get/update/delete a test album, add/list/list-all/remove the generated image, create and cancel a one-day test album share |
| Video | Upload the generated MP4 and poll for a finished transcoding task with a playback URL, for up to 60 seconds |
| Async tasks | Query, batch-query and wait for real task IDs returned by test moves/share saves; require task success |

The suite never selects existing user files as upload/share/mutation fixtures.
Root directories use `aliyunpan-sdk-check-<random-id>` and `Refuse` mode: an
existing directory is not reused. Album fixtures are uploaded into a separate
owned directory in the album drive to avoid copying unrelated files. Cleanup
verifies the exact drive, file ID, name, parent and folder type before deleting
a sandbox. Shares, album memberships, albums and detached trashed test files
are also cleaned up. Cleanup failures fail the workflow.

**Deliberate exclusions:** `clear_recycle_bin`, `clear_recycle_bin_and_wait` and
`device_logout` are never executed. They affect unrelated data or could revoke
the credentials needed for the next scheduled run. The insurance-box endpoint
only reads configuration; the SDK has no insurance-box write methods.

#### Results and capability restrictions

Each run publishes a per-method table in the Actions job summary, plus
`api-report.json` and `api-report.md` in a `web-api-report` artifact retained for
14 days. These contain only method names, statuses and redacted diagnostics,
not tokens, presigned URLs, share links, API response bodies or user file names.

- **PASSED**: the case executed and its assertions passed.
- **FAILED**: a request, response contract, content check or cleanup failed.
  Permissions/membership restrictions are failures, not silently accepted.
- **BLOCKED**: not verified because a prerequisite failed or is absent. For example,
  an account without a distinct resource drive cannot test cross-drive operations;
  a synchronous mutation produces no real task ID for async-task queries.
- **EXCLUDED**: deliberately not executed under the safety policy above.

Independent groups continue after a failure; dependent cases remain blocked.
Any failed case/group makes the process exit nonzero, even if cleanup succeeds.
A successful run means all **executed** cases passed, not that blocked or excluded
APIs were validated. Share/album membership restrictions, saving one's own share,
album-drive folder restrictions and video transcoding availability can differ
between accounts. They remain visible as failures or blocked prerequisites rather
than being mistaken for SDK compatibility.

Share creation responses must contain a nonempty `share_id`; HTTP 200 notices
without it return `Error::UnexpectedResponse` rather than an empty success object.
The report includes only whitelisted field names/types and fixed notice hints
(such as `upgrade_required`), never the notice text or link values. Session renewal
may be rejected independently of ordinary signed operations; recreating a session
is not treated as a successful renewal. For rapid uploads, `rapid_upload` takes
precedence over a returned `upload_id`, since no pending upload session remains.

The test body has a 15-minute budget and cleanup has an additional five minutes.
Unexpected runner termination or an ambiguous mutation response can leave
generated resources behind. The unique sandbox name is printed for manual
recovery; inspect only those test resources, never clear the entire recycle bin.
Quick-transfer links expire naturally. If album membership unexpectedly copies a
fixture outside its sandbox, the case fails and does not blindly delete that copy;
inspect the generated fixture in the album drive manually.

#### Credentials and running the workflow

Use the **`aliyunpan-monitor` GitHub Environment**, restricted to the default
branch, with these environment secrets:

- `ALIYUNPAN_CREDENTIALS`: JSON credentials, including `refresh_token` and a stable
  `device_id`. Seed once; do not overwrite it with an old local token.
- `ALIYUNPAN_SECRET_WRITER`: a GitHub token with permission to update this
  environment's secrets. Prefer a fine-grained token restricted to this repository
  with **Environments: read and write**. The default Actions `GITHUB_TOKEN` cannot
  perform these writes.

The [runner](examples/web_api_check.rs) saves every rotated credential set
immediately using `TokenStore`, an atomic local checkpoint and `gh secret set`,
before subsequent API requests. Failed secret writes are retried, and a separate
write-only recovery step runs after failure. The credential checkpoint is never
uploaded in artifacts and is removed when the job finishes. GitHub and Aliyun
cannot participate in one transaction: a crash or extended persistence outage
after rotation can still require logging in again and reseeding the secret.

All runs share a non-canceling concurrency group. Do not use these rotating
credentials simultaneously in local clients or other workflows. Existing
environment secrets work with the expanded suite without reinitialization.

After deploying to the default branch:

```sh
gh workflow run web-api-check.yml --repo mingcheng/aliyunpan --ref main
gh run list --repo mingcheng/aliyunpan --workflow web-api-check.yml --limit 5
```

The same runner without `--full` retains the limited read-only smoke check.
`--persist-only` republishes a retained checkpoint without another Aliyun refresh.
Neither mode accesses `.env` automatically. The older opt-in tests in
`tests/live.rs` remain for local development and are not the scheduled suite.
Enable GitHub Actions failure notifications; cron execution can be delayed and
public repositories' schedules may be disabled after prolonged inactivity.

### Releases

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
