# Copilot instructions for aliyunpan

Unofficial async Rust SDK (edition 2024, MSRV 1.85) for the Aliyun Drive (alipan.com) **web** API — not the Open Platform API. Endpoints are reverse-engineered and can change without notice.

## Build, test, lint

```sh
cargo fmt --all -- --check                                   # rustfmt.toml: max_width = 120
cargo clippy --locked --all-targets -- -D warnings           # CI also runs with --all-features (needs OpenSSL for native-tls)
cargo test --locked --lib --test mock --example web_api_check # all offline tests
cargo test --locked --doc                                    # README.md is the crate doc; its code blocks are doctests
RUSTDOCFLAGS="-D warnings" cargo doc --locked --no-deps      # CI denies rustdoc warnings
cargo package --locked                                       # verifies the Cargo.toml `include` allowlist
```

`.vscode/tasks.json` wraps these commands (`cargo-check` runs fmt + clippy + lib/mock tests + doctests).

Single test examples:

```sh
cargo test --test mock cross_drive_move_targets_backup_drive
cargo test --lib auth::tests::minimal_json_is_accepted
cargo test --example web_api_check suite::tests::catalog_covers_every_public_async_api_and_helper
cargo test --locked --example web_api_check refresh_if_due    # keepalive tests (name filter)
```

CI also tests `cargo +1.85.0 test --locked --lib --test mock` and `--no-default-features --features native-tls`. Do not use APIs newer than Rust 1.85.

Live tests (`tests/live.rs`) hit the real service. They need a `.env` containing `REFRESH_TOKEN`/`DEVICE_ID`, rotate and rewrite that file, and write tests also need `ALIYUNPAN_WRITE_TEST=1`. Never run them unless explicitly asked.

Releases are published by `crates.yml` on `vX.Y.Z` tags; the tag must equal the `Cargo.toml` version.

## Architecture

- `Client::connect`/`connect_shared` load credentials from a `TokenStore`, generate a `device_id` if missing, and create a signed device session. `Client::refresh_credentials` only refreshes and persists tokens (no device session); it backs the keepalive workflow.
- `Client` (`src/client.rs`) is a cheap `Arc` clone. Shared `State` holds credentials, the device `DeviceKey`, the current signature, the session nonce and an `epoch`. A `recover` mutex serializes token refresh and session rebuilds.
- Every endpoint method is an `impl Client` block spread across `src/api/*.rs`, `upload.rs`, `download.rs`, and `path.rs`. Endpoints call one of:
  - `self.post(path, &json!{..})` — signed (`x-device-id` + `x-signature`) request to the API host. Use this for most endpoints.
  - `self.post_unsigned(Host::.., ..)` — bearer token only (user/account info in `api/user.rs`).
  - `self.send_transfer(|http| ..)` — presigned OSS URLs (part PUT / download GET). No auth headers; retries connect/read timeouts because the bytes are immutable.
- Request flow is `request()` → `ensure_fresh()` → `execute()`. A single `AccessTokenInvalid` triggers `refresh_if_current`; a single `SignatureInvalid` triggers `rebuild_session_if(epoch)`. The token/epoch "seen" checks prevent concurrent tasks from refreshing twice.
- `execute()` retries only HTTP 429/502/503/504 with linear backoff capped at 60 s. **JSON mutations are never replayed on transport errors** (ambiguous outcome). Keep that invariant when touching retries.
- Refresh tokens rotate on every refresh. `refresh_locked` saves to the `TokenStore` immediately, sets `pending_save` on failure, and retries persistence before the next request. Never reorder: a refreshed token must be persisted before any further call.
- `renew_session` signs `nonce + 1` and publishes the new signature only after server success. The server currently rejects it (`DeviceSessionSignatureInvalid`); treat it as a known limitation.
- `error::check_response` turns a response into `Error`. An HTTP 200 whose body carries a non-success `code` is still an `Error::Api`. `ApiErrorKind` classifies codes; callers use `api_kind()`, `needs_relogin()`, `is_not_found()`, `is_already_exists()`.
- Upload (`upload.rs`): `begin_upload` hashes SHA1 plus a `proof_code` derived from the access token, then returns `UploadStart::Rapid` or `Pending(UploadState)`. `UploadState` is serializable but never stores presigned URLs. Upload part 409 counts as success.
- `examples/web_api_check.rs` + `examples/web_api_check/suite.rs` form the scheduled live contract suite, not a demo. Usage: `web_api_check <credentials.json> [--persist-only | --refresh-if-due | --full <fixture-dir>]`. It seeds credentials from the `ALIYUNPAN_CREDENTIALS` env var. Its `GitHubStore` publishes every rotated token immediately via `gh secret set ALIYUNPAN_CREDENTIALS --env aliyunpan-monitor`. Workflows rerun with `--persist-only` after a failure, so a rotated token is never lost. Credential-free unit tests (in `web_api_check.rs` and `suite_tests.rs`, using a fake `gh` script) run via `cargo test --example web_api_check`.
- `examples/quickstart.rs` and `examples/backup.rs` are the user-facing examples linked from the README. `backup.rs` runs `safe_join` on remote paths before writing locally.

## Conventions

- **Adding a public `pub async fn` on `Client`** requires adding its name to `METHODS` in `examples/web_api_check/suite.rs` and a live `report.case("name", ..)`. Otherwise `catalog_covers_every_public_async_api_and_helper` fails. Also add a mock test in `tests/mock.rs`. The catalog test text-scans only the source files listed in its `sources` array, so add new endpoint files there. Local-only accessors go in its `local_or_stream` exemption list instead of `METHODS`.
- README code blocks are doctests compiled and **run** in CI. Blocks that would touch the network or need credentials must be fenced as `rust,no_run`, or only define functions (as the backup snippets do).
- Request bodies are built with `serde_json::json!` inline. Server field names are copied exactly, including odd casing such as `to_parent_fileId` and `ignoreError`. Omit optional markers rather than sending empty strings (`api::set_marker`; `list_uploaded_parts` rejects `""`).
- Response models in `src/models.rs` use `#[serde(default)]` and `#[serde(deserialize_with = "nullable")]` on non-`Option` fields, because the server sends `null` freely. Timestamps stay as RFC3339 `String`s; do not add a date/time dependency.
- Paginated `list_all_*` helpers loop on `next_marker` and `sleep(config.page_delay)` between pages.
- Validate input locally before any request, returning `Error::InvalidInput` without hitting the network (`validate_file_name`, distinct drives for cross-drive ops, part count ≤ `MAX_PARTS`).
- Batch and cross-drive calls return per-item results. `BatchResponse::is_success` means 2xx with no error body; `CrossDriveItem::is_success` means status 201. An outer `Ok` is not overall success.
- Share-creation responses must contain a non-empty `share_id` (`decode_created_share`); otherwise return `Error::UnexpectedResponse`, whose `shape` holds only whitelisted field names and types. Never put tokens, URLs, file names or raw bodies into errors, reports or `Debug` output (`Credentials` debug is redacted).
- Mock tests use `tests/common/mod.rs`: `MockServer::start(handler)`, `Auth::handle` (token + create_session) and `Auth::check` (bearer + signature). Assert on `server.find(path)[i].json()` for exact request bodies and on `server.count(path)` to prove no request was sent. Presigned URLs are built with `req.url("/path")` and served before `Auth::check`.
- Dependencies are deliberately minimal (reqwest, tokio, serde, serde_json, bytes, k256, sha1, md-5, getrandom). TLS is a feature: `rustls` (default) or `native-tls`. `unsafe_code` is forbidden.
- Never commit `.env`, `credentials.json` or tokens. `Cargo.toml` `include` is an allowlist; update it when adding files that the package, README or tests reference (including `include_str!` targets such as `credentials.example.json`). `credentials.example.json` is the committed template and is validated by `auth::tests::example_credentials_file_is_valid`; keep its `device_id` empty so the SDK generates a unique one.
- The live workflows share the `aliyunpan-monitor` environment secrets and the `aliyunpan-monitor-credentials` concurrency group (`cancel-in-progress: false`). The suite must never call `clear_recycle_bin*` or `device_logout`.
