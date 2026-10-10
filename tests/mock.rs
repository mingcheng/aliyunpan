//! Local HTTP integration tests covering authentication recovery, transfers, batches, and path operations.

mod common;

use std::{
    collections::{BTreeMap, HashMap},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

use aliyunpan::{
    ApiErrorKind, CheckNameMode, Client, Config, Credentials, Error, ListOptions, MemoryStore, TokenStore,
    UploadOptions, UploadSource, UploadStart, proof_range,
};
use common::*;
use serde_json::{Value, json};

fn file_get_ok(req: &Request) -> Response {
    let id = req.json()["file_id"].as_str().unwrap_or_default().to_owned();
    Response::json(file_json(&id, "a.bin", "file", "root"))
}

fn bottle_json() -> Value {
    json!({
        "bottleId": 1734102344205721601_u64,
        "bottleName": "Example resource",
        "shareId": "example-share"
    })
}

#[tokio::test]
async fn bottle_endpoints_send_authenticated_empty_posts_and_decode_camel_case() {
    let auth = Auth::default();
    let quota = json!({
        "createBottleLimit": 100, "createBottleUsed": 0,
        "fishBottleLimit": 10, "fishBottleUsed": 3
    });
    let response = quota.clone();
    let server = MockServer::start(move |req| {
        if let Some(response) = auth.handle(req).or_else(|| auth.check(req)) {
            return response;
        }
        assert_eq!(req.method, "POST");
        assert_eq!(req.header("x-device-id"), Some(DEVICE_ID));
        match req.path.as_str() {
            "/adrive/v1/bottle/fish" => Response::json(bottle_json()),
            "/adrive/v1/bottle/getUserLimit" => Response::json(response.clone()),
            _ => panic!("unexpected endpoint"),
        }
    })
    .await;
    let (client, _) = connect(&server).await;
    let limit = client.get_bottle_user_limit().await.unwrap();
    assert_eq!(limit.create_bottle_limit, 100);
    assert_eq!(limit.create_bottle_used, 0);
    assert_eq!(limit.fish_bottle_limit, 10);
    assert_eq!(limit.fish_bottle_used, 3);
    assert_eq!(serde_json::to_value(limit).unwrap(), quota);
    assert_eq!(server.count("/adrive/v1/bottle/fish"), 0);
    let bottle = client.fish_bottle().await.unwrap();
    assert_eq!(bottle.bottle_id, 1734102344205721601);
    assert_eq!(bottle.bottle_name, "Example resource");
    assert_eq!(bottle.share_id, "example-share");
    assert_eq!(serde_json::to_value(&bottle).unwrap(), bottle_json());
    let debug = format!("{bottle:?}");
    for private in ["1734102344205721601", "Example resource", "example-share"] {
        assert!(!debug.contains(private));
    }
    for path in ["/adrive/v1/bottle/fish", "/adrive/v1/bottle/getUserLimit"] {
        assert_eq!(server.count(path), 1);
        assert_eq!(server.find(path)[0].json(), json!({}));
    }
}

#[tokio::test]
async fn bottle_api_errors_are_preserved_without_retrying() {
    for status in [200, 403] {
        let auth = Auth::default();
        let server = MockServer::start(move |req| {
            auth.handle(req).or_else(|| auth.check(req)).unwrap_or_else(|| {
                // Synthetic code: the live quota-exhaustion code is not yet known.
                Response::error(status, "MockBottleQuotaExceeded")
            })
        })
        .await;
        let (client, _) = connect(&server).await;
        for result in [
            client.fish_bottle().await.map(|_| ()),
            client.get_bottle_user_limit().await.map(|_| ()),
        ] {
            let error = result.unwrap_err();
            let api = error.api().unwrap();
            assert_eq!(api.code, "MockBottleQuotaExceeded");
            assert_eq!(api.http_status, status);
        }
        assert_eq!(server.count("/adrive/v1/bottle/fish"), 1);
        assert_eq!(server.count("/adrive/v1/bottle/getUserLimit"), 1);
    }
}

#[tokio::test]
async fn bottle_draw_does_not_retry_transient_statuses_or_timeouts() {
    for status in [200, 429, 502, 503, 504] {
        let auth = Auth::default();
        let server = MockServer::start(move |req| {
            auth.handle(req).or_else(|| auth.check(req)).unwrap_or_else(|| {
                if status == 200 {
                    Response::json(bottle_json()).delayed(Duration::from_millis(600))
                } else {
                    Response::with_status(status, json!({}))
                }
            })
        })
        .await;
        let mut cfg = config(&server.url);
        cfg.request_timeout = Duration::from_millis(200);
        let (client, _) = connect_with(cfg, &server).await;
        let error = client.fish_bottle().await.unwrap_err();
        match status {
            200 => assert!(matches!(error, Error::Network(e) if e.is_timeout())),
            429 => assert!(matches!(error, Error::RateLimited { status: 429, .. })),
            _ => assert!(matches!(error, Error::Http { status: actual, .. } if actual == status)),
        }
        assert_eq!(server.count("/adrive/v1/bottle/fish"), 1);
    }
}

#[tokio::test]
async fn bottle_draw_recovers_from_explicit_authentication_rejection() {
    for code in ["AccessTokenInvalid", "DeviceSessionSignatureInvalid"] {
        let auth = Auth::default();
        let attempts = AtomicUsize::new(0);
        let server = MockServer::start(move |req| {
            if let Some(response) = auth.handle(req).or_else(|| auth.check(req)) {
                return response;
            }
            if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                Response::error(401, code)
            } else {
                Response::json(bottle_json())
            }
        })
        .await;
        let (client, _) = connect(&server).await;
        assert_eq!(client.fish_bottle().await.unwrap().share_id, "example-share");
        assert_eq!(server.count("/adrive/v1/bottle/fish"), 2);
    }
}

#[tokio::test]
async fn bottle_endpoints_reject_invalid_success_shapes_without_leaking_values() {
    for value in [
        Value::Null,
        json!({}),
        json!({"message": "private-notice"}),
        json!({"bottleId": null, "bottleName": "private-name", "shareId": "private-share"}),
        json!({"bottleId": 1734102344205721601_u64, "bottleName": "private-name", "shareId": ""}),
        json!({"bottleId": "private-id", "bottleName": "private-name", "shareId": "private-share"}),
        json!({"createBottleLimit": 100, "createBottleUsed": 0, "fishBottleLimit": 10}),
        json!({"createBottleLimit": 100, "createBottleUsed": 0, "fishBottleLimit": null, "fishBottleUsed": 3}),
        json!({"createBottleLimit": 100, "createBottleUsed": 0, "fishBottleLimit": 10, "fishBottleUsed": -1}),
    ] {
        let auth = Auth::default();
        let server = MockServer::start(move |req| {
            auth.handle(req)
                .or_else(|| auth.check(req))
                .unwrap_or_else(|| Response::json(value.clone()))
        })
        .await;
        let (client, _) = connect(&server).await;
        for result in [
            client.fish_bottle().await.map(|_| ()),
            client.get_bottle_user_limit().await.map(|_| ()),
        ] {
            let error = result.unwrap_err();
            assert!(matches!(error, Error::UnexpectedResponse { .. }));
            let text = format!("{error:?}");
            assert!(!text.contains("private-"));
            assert!(!text.contains("1734102344205721601"));
        }
    }
}

#[tokio::test]
async fn transfer_timeouts_retry_identical_gets_and_part_puts() {
    for upload in [false, true] {
        let auth = Auth::default();
        let attempts = AtomicUsize::new(0);
        let server = MockServer::start(move |req| {
            if let Some(response) = auth.handle(req) {
                return response;
            }
            assert_eq!(req.path, "/transfer");
            assert_eq!(req.method, if upload { "PUT" } else { "GET" });
            if upload {
                assert_eq!(req.body, b"immutable part");
            }
            if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                Response::bytes(200, b"content".to_vec()).delayed(Duration::from_millis(600))
            } else if upload {
                Response::bytes(409, Vec::new())
            } else {
                Response::bytes(200, b"content".to_vec())
            }
        })
        .await;
        let mut cfg = config(&server.url);
        cfg.request_timeout = Duration::from_millis(200);
        cfg.max_retries = 1;
        let (client, _) = connect_with(cfg, &server).await;
        let url = format!("{}/transfer", server.url);
        if upload {
            client
                .upload_part(&url, aliyunpan::Bytes::from_static(b"immutable part"))
                .await
                .unwrap();
        } else {
            assert_eq!(
                client
                    .open_download_url(&url, None)
                    .await
                    .unwrap()
                    .bytes()
                    .await
                    .unwrap(),
                "content"
            );
        }
        assert_eq!(server.count("/transfer"), 2);
    }
}

#[tokio::test]
async fn transfer_timeouts_stop_at_retry_limit() {
    let auth = Auth::default();
    let server = MockServer::start(move |req| {
        auth.handle(req)
            .unwrap_or_else(|| Response::bytes(200, Vec::new()).delayed(Duration::from_millis(600)))
    })
    .await;
    let mut cfg = config(&server.url);
    cfg.request_timeout = Duration::from_millis(200);
    cfg.max_retries = 1;
    let (client, _) = connect_with(cfg, &server).await;
    let error = client
        .upload_part(&format!("{}/part", server.url), aliyunpan::Bytes::from_static(b"same"))
        .await
        .unwrap_err();
    assert!(matches!(error, Error::Network(e) if e.is_timeout()));
    assert_eq!(server.count("/part"), 2);
}

#[tokio::test]
async fn json_mutation_timeouts_are_not_replayed() {
    let auth = Auth::default();
    let server = MockServer::start(move |req| {
        auth.handle(req)
            .unwrap_or_else(|| Response::json(json!({"file_id": "created"})).delayed(Duration::from_millis(600)))
    })
    .await;
    let mut cfg = config(&server.url);
    cfg.request_timeout = Duration::from_millis(200);
    cfg.max_retries = 3;
    let (client, _) = connect_with(cfg, &server).await;
    let error = client
        .create_folder(DRIVE_ID, "root", "test", CheckNameMode::Refuse)
        .await
        .unwrap_err();
    assert!(matches!(error, Error::Network(e) if e.is_timeout()));
    assert_eq!(server.count("/adrive/v2/file/createWithFolders"), 1);
}

#[tokio::test]
async fn share_creation_rejects_success_shaped_notices_without_leaking_values() {
    let auth = Auth::default();
    let server = MockServer::start(move |req| {
        auth.handle(req).unwrap_or_else(|| {
            Response::json(json!({
                "message": "Please upgrade to continue; private-account-info",
                "error": "Private error text must remain redacted",
                "data": {"share_url": "https://private-link.invalid/secret"},
                "private-field-name": "secret-value"
            }))
        })
    })
    .await;
    let (client, _) = connect(&server).await;
    let files = [aliyunpan::DriveFile::new(DRIVE_ID, "generated-file")];
    let results = [
        client
            .create_share_link(DRIVE_ID, &["generated-file"], "abcd", None)
            .await
            .map(|_| ()),
        client
            .create_album_share("generated-album", "abcd", None)
            .await
            .map(|_| ()),
        client.create_fast_share(&files).await.map(|_| ()),
        client
            .create_fast_share_from_ids(DRIVE_ID, &["generated-file"])
            .await
            .map(|_| ()),
    ];
    for result in results {
        let error = result.unwrap_err();
        assert!(matches!(&error, Error::UnexpectedResponse { .. }));
        let text = format!("{error:?}");
        assert!(text.contains("notice=upgrade_required"));
        assert!(text.contains("error=string"));
        assert!(text.contains("data.share_url=string"));
        for secret in [
            "private-account-info",
            "private-link",
            "secret-value",
            "private-field-name",
            "Private error text",
        ] {
            assert!(!text.contains(secret));
        }
    }
}

#[tokio::test]
async fn share_creation_preserves_valid_response_fields() {
    let auth = Auth::default();
    let server = MockServer::start(move |req| {
        auth.handle(req).unwrap_or_else(|| {
            Response::json(json!({
                "share_id": "test-share", "share_url": "https://example.invalid/s/test-share"
            }))
        })
    })
    .await;
    let (client, _) = connect(&server).await;
    assert_eq!(
        client
            .create_share_link(DRIVE_ID, &["file"], "abcd", None)
            .await
            .unwrap()
            .share_id,
        "test-share"
    );
    assert_eq!(
        client.create_album_share("album", "abcd", None).await.unwrap().share_id,
        "test-share"
    );
    assert_eq!(
        client
            .create_fast_share_from_ids(DRIVE_ID, &["file"])
            .await
            .unwrap()
            .share_id,
        "test-share"
    );
}

#[tokio::test]
async fn renewal_signature_rejection_is_not_hidden_by_session_recreation() {
    let auth = Arc::new(Auth::default());
    let handler = auth.clone();
    let server = MockServer::start(move |req| {
        if let Some(response) = handler.handle(req) {
            return response;
        }
        if req.path == "/users/v1/users/device/renew_session" {
            return Response::error(400, "DeviceSessionSignatureInvalid");
        }
        handler.check(req).unwrap_or_else(|| file_get_ok(req))
    })
    .await;
    let (client, _) = connect(&server).await;
    assert_eq!(
        client.renew_session().await.unwrap_err().api_kind(),
        Some(ApiErrorKind::SignatureInvalid)
    );
    assert_eq!(server.count("/users/v1/users/device/renew_session"), 1);
    assert_eq!(auth.sessions.load(Ordering::SeqCst), 1);
    client.get_file(DRIVE_ID, "test-file").await.unwrap();
    assert_eq!(auth.sessions.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn renewal_signs_next_nonce_and_publishes_it_to_concurrent_clones() {
    use k256::{
        ecdsa::{Signature, VerifyingKey, signature::hazmat::PrehashVerifier},
        sha2::{Digest, Sha256},
    };

    fn unhex(value: &str) -> Vec<u8> {
        (0..value.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&value[i..i + 2], 16).unwrap())
            .collect()
    }

    let auth = Auth::default();
    let session = Mutex::new((None::<VerifyingKey>, 0u64));
    let server = MockServer::start(move |req| {
        if req.path == "/users/v1/users/device/create_session" {
            let public = req.json()["pubKey"].as_str().unwrap().to_owned();
            *session.lock().unwrap() = (Some(VerifyingKey::from_sec1_bytes(&unhex(&public[2..])).unwrap()), 0);
        }
        if let Some(response) = auth.handle(req) {
            return response;
        }
        assert_eq!(req.header("authorization"), Some(auth.token().as_str()));
        assert_eq!(req.header("x-device-id"), Some(DEVICE_ID));
        let mut state = session.lock().unwrap();
        let renewing = req.path == "/users/v1/users/device/renew_session";
        let nonce = state.1 + u64::from(renewing);
        let digest = Sha256::digest(format!("{}:{DEVICE_ID}:{USER_ID}:{nonce}", Config::default().app_id).as_bytes());
        let signature = req.header("x-signature").unwrap();
        state
            .0
            .as_ref()
            .unwrap()
            .verify_prehash(&digest, &Signature::from_slice(&unhex(&signature[..128])).unwrap())
            .unwrap();
        if renewing {
            assert_eq!(req.json(), json!({}));
            state.1 = nonce;
            Response::json(json!({"result": true, "success": true}))
        } else {
            file_get_ok(req)
        }
    })
    .await;
    let (client, _) = connect(&server).await;
    client.renew_session().await.unwrap();
    client.get_file(DRIVE_ID, "test-file").await.unwrap();
    let clone = client.clone();
    let (first, second) = tokio::join!(client.renew_session(), clone.renew_session());
    first.unwrap();
    second.unwrap();
    clone.get_file(DRIVE_ID, "test-file").await.unwrap();
    assert_eq!(server.count("/users/v1/users/device/create_session"), 1);
    client.recreate_session().await.unwrap();
    client.renew_session().await.unwrap();
    client.get_file(DRIVE_ID, "test-file").await.unwrap();
    assert_eq!(server.count("/users/v1/users/device/create_session"), 2);
    assert_eq!(server.count("/users/v1/users/device/renew_session"), 4);
}

#[tokio::test]
async fn ambiguous_renewal_errors_do_not_replay_or_publish_the_candidate_signature() {
    for response in [
        Response::error(503, "Unavailable"),
        Response::json(json!({"result": false, "success": true})),
        Response::bytes(200, b"invalid-json".to_vec()),
        Response::json(json!({"result": true, "success": true})).delayed(Duration::from_millis(600)),
    ] {
        let auth = Arc::new(Auth::default());
        let handler = auth.clone();
        let server = MockServer::start(move |req| {
            if let Some(response) = handler.handle(req) {
                return response;
            }
            if req.path == "/users/v1/users/device/renew_session" {
                return response.clone();
            }
            handler.check(req).unwrap_or_else(|| file_get_ok(req))
        })
        .await;
        let mut cfg = config(&server.url);
        cfg.request_timeout = Duration::from_millis(200);
        let (client, _) = connect_with(cfg, &server).await;
        assert!(client.renew_session().await.is_err());
        assert_eq!(server.count("/users/v1/users/device/renew_session"), 1);
        assert_eq!(auth.sessions.load(Ordering::SeqCst), 1);
        client.get_file(DRIVE_ID, "test-file").await.unwrap();
        assert_eq!(auth.sessions.load(Ordering::SeqCst), 2);
    }
}

#[tokio::test]
async fn canceled_renewal_invalidates_session_before_releasing_locks() {
    let auth = Arc::new(Auth::default());
    let handler = auth.clone();
    let received = Arc::new(tokio::sync::Notify::new());
    let notify = received.clone();
    let server = MockServer::start(move |req| {
        if let Some(response) = handler.handle(req) {
            return response;
        }
        if req.path == "/users/v1/users/device/renew_session" {
            notify.notify_one();
            return Response::json(json!({"result": true, "success": true})).delayed(Duration::from_secs(1));
        }
        handler.check(req).unwrap_or_else(|| file_get_ok(req))
    })
    .await;
    let (client, _) = connect(&server).await;
    let clone = client.clone();
    let task = tokio::spawn(async move { clone.renew_session().await });
    tokio::time::timeout(Duration::from_secs(2), received.notified())
        .await
        .unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    client.get_file(DRIVE_ID, "test-file").await.unwrap();
    assert_eq!(auth.sessions.load(Ordering::SeqCst), 2);
    assert_eq!(server.count("/users/v1/users/device/renew_session"), 1);
}

struct FailingStore {
    inner: MemoryStore,
    fail: AtomicBool,
}

impl TokenStore for FailingStore {
    fn load(&self) -> aliyunpan::Result<Option<Credentials>> {
        self.inner.load()
    }

    fn save(&self, credentials: &Credentials) -> aliyunpan::Result<()> {
        if self.fail.load(Ordering::SeqCst) {
            return Err(Error::Io(std::io::Error::other("disk unavailable")));
        }
        self.inner.save(credentials)
    }
}

#[tokio::test]
async fn failed_persistence_retains_rotated_credentials_and_retries_save() {
    let auth = Arc::new(Auth::default());
    let a = auth.clone();
    let server = MockServer::start(move |req| a.handle(req).unwrap_or_else(|| Response::error(404, "x"))).await;
    let mut creds = initial_credentials();
    creds.access_token = "at-1".into();
    creds.user_id = USER_ID.into();
    creds.expires_at = i64::MAX / 2;
    let store = Arc::new(FailingStore {
        inner: MemoryStore::new(creds.clone()),
        fail: AtomicBool::new(false),
    });
    let client = Client::connect_shared(config(&server.url), store.clone())
        .await
        .unwrap();
    let before = client.credentials().await;
    let persisted = store.load().unwrap();
    store.fail.store(true, Ordering::SeqCst);
    assert!(matches!(client.force_refresh().await, Err(Error::Io(_))));
    let rotated = client.credentials().await;
    assert_ne!(rotated.refresh_token, before.refresh_token);
    assert_eq!(store.load().unwrap(), persisted);
    assert!(matches!(client.access_token().await, Err(Error::Io(_))));
    assert!(matches!(client.force_refresh().await, Err(Error::Io(_))));
    assert_eq!(auth.refreshes.load(Ordering::SeqCst), 2);

    store.fail.store(false, Ordering::SeqCst);
    assert_eq!(client.access_token().await.unwrap(), rotated.access_token);
    assert_eq!(store.load().unwrap(), Some(rotated));
    client.force_refresh().await.unwrap();
    assert_eq!(auth.refreshes.load(Ordering::SeqCst), 3);
    assert_eq!(store.load().unwrap(), Some(client.credentials().await));
}

#[tokio::test]
async fn connect_refreshes_token_and_creates_session() {
    let auth = Arc::new(Auth::default());
    let a = auth.clone();
    let server =
        MockServer::start(move |req| a.handle(req).unwrap_or_else(|| Response::error(404, "NotFound.View"))).await;
    let (client, store) = connect(&server).await;

    assert_eq!(auth.refreshes.load(Ordering::SeqCst), 1);
    assert_eq!(auth.sessions.load(Ordering::SeqCst), 1);
    assert_eq!(client.user_id().await, USER_ID);
    assert_eq!(client.default_drive_id().await, DRIVE_ID);

    let saved = store.get().unwrap();
    assert_eq!(saved.refresh_token, "rt-1", "rotated refresh token must be persisted");
    assert_eq!(saved.device_id, DEVICE_ID, "device id must stay stable");

    let token_req = &server.find("/v2/account/token")[0];
    assert!(token_req.header("authorization").is_none());
    let body = token_req.json();
    assert_eq!(body["grant_type"], "refresh_token");
    assert_eq!(body["api_id"], "pJZInNHN2dZWk8qg");

    let session_req = &server.find("/users/v1/users/device/create_session")[0];
    assert_eq!(session_req.header("authorization"), Some("Bearer at-1"));
    assert_eq!(session_req.header("x-device-id"), Some(DEVICE_ID));
    assert_eq!(session_req.header("referer"), Some("https://www.alipan.com/"));
    assert_eq!(session_req.header("origin"), Some("https://www.alipan.com"));
    assert_eq!(
        session_req.header("content-type"),
        Some("application/json;charset=UTF-8")
    );
    let sig = session_req.header("x-signature").unwrap();
    assert_eq!(sig.len(), 130);
    assert!(sig.ends_with("01"));
    let pub_key = session_req.json()["pubKey"].as_str().unwrap().to_owned();
    assert_eq!(pub_key.len(), 68);
    assert!(pub_key.starts_with("0402") || pub_key.starts_with("0403"));
}

#[tokio::test]
async fn connect_skips_refresh_when_token_is_fresh() {
    let auth = Arc::new(Auth::default());
    auth.refreshes.store(5, Ordering::SeqCst);
    let a = auth.clone();
    let server = MockServer::start(move |req| a.handle(req).unwrap_or_else(|| Response::error(404, "x"))).await;
    let mut creds = initial_credentials();
    creds.access_token = "at-5".into();
    creds.user_id = USER_ID.into();
    creds.expires_at = i64::MAX / 2;
    let client = Client::connect(config(&server.url), MemoryStore::new(creds))
        .await
        .unwrap();
    assert_eq!(auth.refreshes.load(Ordering::SeqCst), 5);
    assert_eq!(server.count("/v2/account/token"), 0);
    assert_eq!(client.access_token().await.unwrap(), "at-5");
}

#[tokio::test]
async fn connect_reports_invalid_refresh_token() {
    let server = MockServer::start(|_| Response::error(400, "InvalidParameter.RefreshToken")).await;
    let err = Client::connect(config(&server.url), MemoryStore::new(initial_credentials()))
        .await
        .unwrap_err();
    assert!(err.needs_relogin(), "{err}");
}

#[tokio::test]
async fn connect_requires_credentials() {
    let err = Client::connect(Config::default(), MemoryStore::default())
        .await
        .unwrap_err();
    assert!(matches!(err, Error::NotLoggedIn));
}

#[tokio::test]
async fn generates_device_id_when_missing() {
    let auth = Arc::new(Auth::default());
    let a = auth.clone();
    let server = MockServer::start(move |req| a.handle(req).unwrap_or_else(|| Response::error(404, "x"))).await;
    let mut creds = initial_credentials();
    creds.device_id.clear();
    let store = Arc::new(MemoryStore::new(creds));
    Client::connect_shared(config(&server.url), store.clone())
        .await
        .unwrap();
    let id = store.get().unwrap().device_id;
    assert_eq!(id.len(), 32);
    assert_eq!(
        server.find("/users/v1/users/device/create_session")[0].header("x-device-id"),
        Some(id.as_str())
    );
}

#[tokio::test]
async fn refreshes_token_when_server_rejects_it() {
    let auth = Arc::new(Auth::default());
    let a = auth.clone();
    let rejected = Arc::new(AtomicUsize::new(0));
    let r = rejected.clone();
    let server = MockServer::start(move |req| {
        if let Some(resp) = a.handle(req) {
            return resp;
        }
        if req.path == "/v2/file/get" && r.fetch_add(1, Ordering::SeqCst) == 0 {
            return Response::error(401, "AccessTokenInvalid");
        }
        a.check(req).unwrap_or_else(|| file_get_ok(req))
    })
    .await;
    let (client, store) = connect(&server).await;

    let item = client.get_file(DRIVE_ID, "f1").await.unwrap();
    assert_eq!(item.file_id, "f1");
    assert_eq!(auth.refreshes.load(Ordering::SeqCst), 2);
    assert_eq!(store.get().unwrap().refresh_token, "rt-2");
    let gets = server.find("/v2/file/get");
    assert_eq!(gets.len(), 2);
    assert_eq!(gets[1].header("authorization"), Some("Bearer at-2"));
}

#[tokio::test]
async fn concurrent_requests_refresh_only_once() {
    let auth = Arc::new(Auth::default());
    let a = auth.clone();
    let server = MockServer::start(move |req| {
        if let Some(resp) = a.handle(req) {
            return resp;
        }
        if req.header("authorization") == Some("Bearer at-1") {
            return Response::error(401, "AccessTokenInvalid");
        }
        a.check(req).unwrap_or_else(|| file_get_ok(req))
    })
    .await;
    let (client, _) = connect(&server).await;

    let mut set = tokio::task::JoinSet::new();
    for i in 0..8 {
        let c = client.clone();
        set.spawn(async move { c.get_file(DRIVE_ID, &format!("f{i}")).await });
    }
    while let Some(res) = set.join_next().await {
        res.unwrap().unwrap();
    }
    assert_eq!(
        auth.refreshes.load(Ordering::SeqCst),
        2,
        "only one extra refresh expected"
    );
}

#[tokio::test]
async fn recreates_session_on_invalid_signature() {
    let auth = Arc::new(Auth::default());
    let a = auth.clone();
    let rejected = Arc::new(AtomicUsize::new(0));
    let r = rejected.clone();
    let server = MockServer::start(move |req| {
        if let Some(resp) = a.handle(req) {
            return resp;
        }
        if r.fetch_add(1, Ordering::SeqCst) == 0 {
            return Response::error(400, "DeviceSessionSignatureInvalid");
        }
        a.check(req).unwrap_or_else(|| file_get_ok(req))
    })
    .await;
    let (client, _) = connect(&server).await;

    client.get_file(DRIVE_ID, "f1").await.unwrap();
    assert_eq!(auth.sessions.load(Ordering::SeqCst), 2);
    assert_eq!(auth.refreshes.load(Ordering::SeqCst), 1);
    let gets = server.find("/v2/file/get");
    assert_ne!(gets[0].header("x-signature"), gets[1].header("x-signature"));
}

#[tokio::test]
async fn unsigned_endpoints_omit_signature_headers() {
    let auth = Arc::new(Auth::default());
    let a = auth.clone();
    let server = MockServer::start(move |req| {
        a.handle(req).unwrap_or_else(|| {
            Response::json(json!({ "user_id": USER_ID, "default_drive_id": DRIVE_ID, "resource_drive_id": "res-1" }))
        })
    })
    .await;
    let (client, _) = connect(&server).await;
    let info = client.get_user_info().await.unwrap();
    assert_eq!(info.resource_drive_id, "res-1");
    let req = &server.find("/v2/user/get")[0];
    assert_eq!(req.header("authorization"), Some("Bearer at-1"));
    assert!(req.header("x-signature").is_none());
    assert_eq!(req.body, b"{}");
}

#[tokio::test]
async fn retries_rate_limited_requests() {
    let auth = Arc::new(Auth::default());
    let a = auth.clone();
    let hits = Arc::new(AtomicUsize::new(0));
    let h = hits.clone();
    let server = MockServer::start(move |req| {
        if let Some(resp) = a.handle(req) {
            return resp;
        }
        match h.fetch_add(1, Ordering::SeqCst) {
            0 => Response::bytes(429, Vec::new()).header("retry-after", "0"),
            1 => Response::bytes(502, b"Bad Gateway".to_vec()),
            _ => file_get_ok(req),
        }
    })
    .await;
    let (client, _) = connect(&server).await;
    client.get_file(DRIVE_ID, "f1").await.unwrap();
    assert_eq!(hits.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn gives_up_after_max_retries() {
    let auth = Arc::new(Auth::default());
    let a = auth.clone();
    let server = MockServer::start(move |req| a.handle(req).unwrap_or_else(|| Response::bytes(429, Vec::new()))).await;
    let (client, _) = connect(&server).await;
    let err = client.get_file(DRIVE_ID, "f1").await.unwrap_err();
    assert!(matches!(err, Error::RateLimited { status: 429, .. }), "{err}");
    assert_eq!(server.count("/v2/file/get"), 4);
}

#[tokio::test]
async fn api_errors_keep_code_and_status() {
    let auth = Arc::new(Auth::default());
    let a = auth.clone();
    let server = MockServer::start(move |req| {
        a.handle(req).unwrap_or_else(|| {
            Response::with_status(
                404,
                json!({ "code": "NotFound.File", "message": "not found", "display_message": "文件不存在" }),
            )
        })
    })
    .await;
    let (client, _) = connect(&server).await;
    let err = client.get_file(DRIVE_ID, "missing").await.unwrap_err();
    assert!(err.is_not_found());
    let api = err.api().unwrap();
    assert_eq!(api.http_status, 404);
    assert_eq!(api.code, "NotFound.File");
    assert_eq!(api.user_message(), "文件不存在");
}

#[tokio::test]
async fn error_code_in_http_200_body_is_detected() {
    let auth = Arc::new(Auth::default());
    let a = auth.clone();
    let server = MockServer::start(move |req| {
        a.handle(req)
            .unwrap_or_else(|| Response::json(json!({ "code": "FeatureTemporaryDisabled", "message": "x" })))
    })
    .await;
    let (client, _) = connect(&server).await;
    let err = client.get_albums_info().await.unwrap_err();
    assert_eq!(err.api_kind(), Some(ApiErrorKind::FeatureDisabled));
}

#[derive(Default)]
struct UploadMock {
    parts: Mutex<BTreeMap<u32, Vec<u8>>>,
    fail_once: Mutex<Option<u32>>,
    rapid: bool,
}

fn upload_server(auth: Arc<Auth>, mock: Arc<UploadMock>) -> impl Fn(&Request) -> Response + Send + Sync + 'static {
    move |req| {
        if let Some(resp) = auth.handle(req) {
            return resp;
        }
        if let Some(part) = req.path.strip_prefix("/oss/part/") {
            let n: u32 = part.split('?').next().unwrap().parse().unwrap();
            let mut fail = mock.fail_once.lock().unwrap();
            if *fail == Some(n) && !req.path.contains("fresh") {
                *fail = None;
                return Response::bytes(403, b"<Error>AccessDenied</Error>".to_vec());
            }
            mock.parts.lock().unwrap().insert(n, req.body.clone());
            return Response::bytes(200, Vec::new());
        }
        if let Some(resp) = auth.check(req) {
            return resp;
        }
        let body = req.json();
        match req.path.as_str() {
            "/adrive/v2/file/createWithFolders" => {
                let parts: Vec<Value> = body["part_info_list"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|p| json!({ "part_number": p["part_number"], "upload_url": req.url(&format!("/oss/part/{}", p["part_number"])) }))
                    .collect();
                Response::json(json!({
                    "drive_id": DRIVE_ID, "file_id": "f-up", "parent_file_id": body["parent_file_id"],
                    "file_name": body["name"], "type": "file", "upload_id": "u-1",
                    "rapid_upload": mock.rapid, "part_info_list": if mock.rapid { json!([]) } else { json!(parts) },
                }))
            }
            "/v2/file/get_upload_url" => {
                let parts: Vec<Value> = body["part_info_list"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|p| json!({ "part_number": p["part_number"], "upload_url": req.url(&format!("/oss/part/{}?fresh=1", p["part_number"])) }))
                    .collect();
                Response::json(json!({ "upload_id": "u-1", "part_info_list": parts }))
            }
            "/v2/file/complete" => {
                let data: Vec<u8> = mock.parts.lock().unwrap().values().flatten().copied().collect();
                let mut item = file_json("f-up", "data.bin", "file", "root");
                item["size"] = data.len().into();
                item["content_hash"] = sha1_upper(&data).into();
                Response::json(item)
            }
            "/v2/file/get" => {
                let mut item = file_json("f-up", "data.bin", "file", "root");
                item["size"] = 10.into();
                Response::json(item)
            }
            _ => Response::error(404, "NotFound.View"),
        }
    }
}

#[tokio::test]
async fn multipart_upload_round_trip() {
    let auth = Arc::new(Auth::default());
    let mock = Arc::new(UploadMock::default());
    let server = MockServer::start(upload_server(auth.clone(), mock.clone())).await;
    let mut cfg = config(&server.url);
    cfg.chunk_size = 4;
    let (client, _) = connect_with(cfg, &server).await;

    let data = b"0123456789";
    let out = client
        .upload(
            DRIVE_ID,
            "root",
            "data.bin",
            UploadSource::Memory(data),
            &UploadOptions::default(),
        )
        .await
        .unwrap();
    assert!(!out.rapid);
    assert_eq!(out.file.size, 10);
    assert!(out.file.sha1_matches(&sha1_upper(data)));

    let parts = mock.parts.lock().unwrap().clone();
    assert_eq!(parts.len(), 3);
    assert_eq!(parts[&1], b"0123");
    assert_eq!(parts[&3], b"89");

    let create = server.find("/adrive/v2/file/createWithFolders")[0].json();
    assert_eq!(create["size"], 10);
    assert_eq!(create["content_hash"], sha1_upper(data));
    assert_eq!(create["check_name_mode"], "refuse");
    assert_eq!(create["proof_version"], "v1");
    assert_eq!(create["part_info_list"].as_array().unwrap().len(), 3);
    let (start, end) = proof_range("at-1", 10).unwrap();
    assert_eq!(create["proof_code"], base64(&data[start as usize..end as usize]));

    let puts = server.find("/oss/part/");
    assert_eq!(
        puts.iter()
            .map(|put| put.path.split('/').next_back().unwrap().parse::<u32>().unwrap())
            .collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    for put in puts {
        assert_eq!(put.method, "PUT");
        assert!(put.header("authorization").is_none());
        assert!(put.header("content-type").is_none());
        assert!(put.header("content-length").is_some());
        assert_eq!(put.header("referer"), Some("https://www.alipan.com/"));
    }
    let complete = server.find("/v2/file/complete")[0].json();
    assert_eq!(complete["upload_id"], "u-1");
    assert_eq!(complete["ignoreError"], true);
}

#[tokio::test]
async fn upload_refreshes_expired_part_url() {
    let auth = Arc::new(Auth::default());
    let mock = Arc::new(UploadMock {
        fail_once: Mutex::new(Some(2)),
        ..Default::default()
    });
    let server = MockServer::start(upload_server(auth, mock.clone())).await;
    let mut cfg = config(&server.url);
    cfg.chunk_size = 4;
    let (client, _) = connect_with(cfg, &server).await;

    let data = b"0123456789";
    client
        .upload(
            DRIVE_ID,
            "root",
            "data.bin",
            UploadSource::Memory(data),
            &UploadOptions::default(),
        )
        .await
        .unwrap();
    let refresh = server.find("/v2/file/get_upload_url");
    assert_eq!(refresh.len(), 1);
    let requested: Vec<u64> = refresh[0].json()["part_info_list"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["part_number"].as_u64().unwrap())
        .collect();
    assert_eq!(requested, vec![2, 3]);
    assert_eq!(
        mock.parts
            .lock()
            .unwrap()
            .values()
            .flatten()
            .copied()
            .collect::<Vec<_>>(),
        data
    );
}

#[tokio::test]
async fn upload_recomputes_proof_after_token_refresh() {
    let auth = Arc::new(Auth::default());
    let requests = Arc::new(AtomicUsize::new(0));
    let attempts = requests.clone();
    let data: Vec<u8> = (0..4096).map(|index| (index % 251) as u8).collect();
    let source = data.clone();
    let server = MockServer::start(move |req| {
        if let Some(response) = auth.handle(req) {
            return response;
        }
        if let Some(response) = auth.check(req) {
            return response;
        }
        if req.path != "/adrive/v2/file/createWithFolders" {
            return Response::error(404, "NotFound.View");
        }
        if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
            return Response::error(401, "AccessTokenInvalid");
        }
        let token = req.header("authorization").unwrap().strip_prefix("Bearer ").unwrap();
        let (start, end) = proof_range(token, source.len() as u64).unwrap();
        if req.json()["proof_code"] != base64(&source[start as usize..end as usize]) {
            return Response::error(400, "InvalidRapidProof");
        }
        Response::json(json!({ "drive_id": DRIVE_ID, "file_id": "f-up", "upload_id": "u-1" }))
    })
    .await;
    let (client, _) = connect(&server).await;
    let result = client
        .begin_upload(
            DRIVE_ID,
            "root",
            "data.bin",
            UploadSource::Memory(&data),
            &UploadOptions::default(),
        )
        .await
        .unwrap();
    assert!(matches!(result, UploadStart::Pending(_)));
    assert_eq!(requests.load(Ordering::SeqCst), 3);
    assert_eq!(server.count("/v2/account/token"), 2);
}

#[tokio::test]
async fn resume_upload_after_restart() {
    let auth = Arc::new(Auth::default());
    let mock = Arc::new(UploadMock::default());
    let server = MockServer::start(upload_server(auth, mock.clone())).await;
    let mut cfg = config(&server.url);
    cfg.chunk_size = 4;
    let (client, _) = connect_with(cfg, &server).await;

    let data = b"0123456789";
    let UploadStart::Pending(state) = client
        .begin_upload(
            DRIVE_ID,
            "root",
            "data.bin",
            UploadSource::Memory(data),
            &UploadOptions::default(),
        )
        .await
        .unwrap()
    else {
        panic!("expected pending upload");
    };
    // Simulate a restart: part 1 is uploaded, but URLs were not persisted.
    mock.parts.lock().unwrap().insert(1, b"0123".to_vec());
    let mut state: aliyunpan::UploadState = serde_json::from_str(&{
        let mut s = state.clone();
        s.completed_parts.insert(1);
        serde_json::to_string(&s).unwrap()
    })
    .unwrap();

    let mut progress = Vec::new();
    let file = client
        .resume_upload(&mut state, UploadSource::Memory(data), |s| {
            progress.push(s.completed_parts.len())
        })
        .await
        .unwrap();
    assert_eq!(file.size, 10);
    assert_eq!(progress, vec![2, 3]);
    assert_eq!(server.count("/oss/part/1"), 0);
    assert_eq!(server.count("/v2/file/get_upload_url"), 1);

    let err = client
        .resume_upload(&mut state, UploadSource::Memory(b"012345678X"), |_| {})
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Integrity { .. }));

    let mut wrong = state.clone();
    wrong.local_size = 11;
    let err = client
        .resume_upload(&mut wrong, UploadSource::Memory(data), |_| {})
        .await
        .unwrap_err();
    assert!(matches!(err, Error::InvalidInput(_)));
}

#[tokio::test]
async fn rapid_upload_skips_data_transfer() {
    let auth = Arc::new(Auth::default());
    let mock = Arc::new(UploadMock {
        rapid: true,
        ..Default::default()
    });
    let server = MockServer::start(upload_server(auth, mock)).await;
    let (client, _) = connect(&server).await;

    let out = client
        .upload(
            DRIVE_ID,
            "root",
            "data.bin",
            UploadSource::Memory(b"0123456789"),
            &UploadOptions::default(),
        )
        .await
        .unwrap();
    assert!(out.rapid);
    assert_eq!(out.file.file_id, "f-up");
    assert_eq!(server.count("/oss/part/"), 0);
    assert_eq!(server.count("/v2/file/complete"), 0);
}

#[tokio::test]
async fn upload_reports_existing_name_in_refuse_mode() {
    let auth = Arc::new(Auth::default());
    let a = auth.clone();
    let server = MockServer::start(move |req| {
        a.handle(req).unwrap_or_else(|| {
            Response::json(json!({ "drive_id": DRIVE_ID, "file_id": "old", "file_name": "data.bin", "type": "file", "exist": true, "rapid_upload": false }))
        })
    })
    .await;
    let (client, _) = connect(&server).await;
    let err = client
        .upload(
            DRIVE_ID,
            "root",
            "data.bin",
            UploadSource::Memory(b"x"),
            &UploadOptions::default(),
        )
        .await
        .unwrap_err();
    assert!(err.is_already_exists(), "{err}");
}

#[tokio::test]
async fn upload_file_from_disk() {
    let auth = Arc::new(Auth::default());
    let mock = Arc::new(UploadMock::default());
    let server = MockServer::start(upload_server(auth, mock.clone())).await;
    let (client, _) = connect(&server).await;

    let dir = std::env::temp_dir().join(format!("aliyunpan-mock-{}", std::process::id()));
    tokio::fs::create_dir_all(&dir).await.unwrap();
    let path = dir.join("upload-me.txt");
    tokio::fs::write(&path, b"disk content").await.unwrap();
    let opts = UploadOptions {
        check_name_mode: CheckNameMode::Overwrite,
        chunk_size: None,
    };
    client.upload_file(DRIVE_ID, "root", &path, &opts).await.unwrap();
    let create = server.find("/adrive/v2/file/createWithFolders")[0].json();
    assert_eq!(create["name"], "upload-me.txt");
    assert_eq!(create["check_name_mode"], "overwrite");
    assert_eq!(mock.parts.lock().unwrap()[&1], b"disk content");
    tokio::fs::remove_dir_all(&dir).await.unwrap();
}

#[tokio::test]
async fn rejects_invalid_names_before_request() {
    let auth = Arc::new(Auth::default());
    let a = auth.clone();
    let server = MockServer::start(move |req| a.handle(req).unwrap_or_else(|| Response::error(500, "x"))).await;
    let (client, _) = connect(&server).await;
    let err = client
        .create_folder(DRIVE_ID, "root", "a/b", CheckNameMode::Refuse)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::InvalidInput(_)));
    assert_eq!(server.count("/adrive/v2/file/createWithFolders"), 0);
}

fn download_server(
    auth: Arc<Auth>,
    data: Vec<u8>,
    hash: String,
    blocked: bool,
) -> impl Fn(&Request) -> Response + Send + Sync + 'static {
    move |req| {
        if let Some(resp) = auth.handle(req) {
            return resp;
        }
        if req.path == "/data/blob" {
            if req.header("referer") != Some("https://www.aliyundrive.com/") {
                return Response::bytes(403, Vec::new());
            }
            if let Some(range) = req.header("range").and_then(|r| r.strip_prefix("bytes=")) {
                let (s, e) = range.split_once('-').unwrap();
                let (s, e): (usize, usize) = (s.parse().unwrap(), e.parse().unwrap());
                return Response::bytes(206, data[s..=e].to_vec());
            }
            return Response::bytes(200, data.clone());
        }
        if let Some(resp) = auth.check(req) {
            return resp;
        }
        match req.path.as_str() {
            "/v2/file/get_download_url" => {
                let url = if blocked {
                    "https://pds-system-file.oss-cn-beijing.aliyuncs.com/illegal.mp4".to_owned()
                } else {
                    req.url("/data/blob")
                };
                Response::json(json!({ "method": "GET", "url": url, "size": data.len() }))
            }
            "/v2/file/get" => {
                let mut item = file_json("f-dl", "blob.bin", "file", "root");
                item["size"] = data.len().into();
                item["content_hash"] = hash.clone().into();
                Response::json(item)
            }
            _ => Response::error(404, "NotFound.View"),
        }
    }
}

#[tokio::test]
async fn download_full_range_and_to_file() {
    let data: Vec<u8> = (0..=255u8).cycle().take(5000).collect();
    let auth = Arc::new(Auth::default());
    let server = MockServer::start(download_server(
        auth,
        data.clone(),
        sha1_upper(&data).to_lowercase(),
        false,
    ))
    .await;
    let (client, _) = connect(&server).await;

    let full = client.download(DRIVE_ID, "f-dl", None).await.unwrap();
    assert_eq!(full.status(), 200);
    assert_eq!(&full.bytes().await.unwrap()[..], &data[..]);

    let mut part = client.download(DRIVE_ID, "f-dl", Some(100..612)).await.unwrap();
    assert_eq!(part.status(), 206);
    let mut got = Vec::new();
    while let Some(chunk) = part.chunk().await.unwrap() {
        got.extend_from_slice(&chunk);
    }
    assert_eq!(got, &data[100..612]);
    assert_eq!(
        server.find("/data/blob").last().unwrap().header("range"),
        Some("bytes=100-611")
    );
    assert!(
        server
            .find("/data/blob")
            .iter()
            .all(|r| r.header("authorization").is_none())
    );

    let dir = std::env::temp_dir().join(format!("aliyunpan-dl-{}", std::process::id()));
    tokio::fs::create_dir_all(&dir).await.unwrap();
    let dest = dir.join("blob.bin");
    let meta = client.download_to_file(DRIVE_ID, "f-dl", &dest).await.unwrap();
    assert_eq!(meta.size, 5000);
    assert_eq!(tokio::fs::read(&dest).await.unwrap(), data);
    tokio::fs::remove_dir_all(&dir).await.unwrap();

    assert!(matches!(
        client.download(DRIVE_ID, "f-dl", Some(5..5)).await,
        Err(Error::InvalidInput(_))
    ));
}

#[tokio::test]
async fn download_to_file_detects_corruption() {
    let data = b"payload".to_vec();
    let auth = Arc::new(Auth::default());
    let server = MockServer::start(download_server(auth, data, sha1_upper(b"other"), false)).await;
    let (client, _) = connect(&server).await;

    let dir = std::env::temp_dir().join(format!("aliyunpan-bad-{}", std::process::id()));
    tokio::fs::create_dir_all(&dir).await.unwrap();
    let dest = dir.join("blob.bin");
    let err = client.download_to_file(DRIVE_ID, "f-dl", &dest).await.unwrap_err();
    assert!(matches!(err, Error::Integrity { .. }), "{err}");
    assert!(!dest.exists());
    assert!(!dir.join("blob.bin.part").exists());
    tokio::fs::remove_dir_all(&dir).await.unwrap();
}

#[tokio::test]
async fn download_rejects_blocked_resource() {
    let auth = Arc::new(Auth::default());
    let server = MockServer::start(download_server(auth, Vec::new(), String::new(), true)).await;
    let (client, _) = connect(&server).await;
    assert!(matches!(
        client.download(DRIVE_ID, "f-dl", None).await,
        Err(Error::Blocked)
    ));
}

#[tokio::test]
async fn batch_reports_each_sub_result() {
    let auth = Arc::new(Auth::default());
    let a = auth.clone();
    let server = MockServer::start(move |req| {
        if let Some(resp) = a.handle(req) {
            return resp;
        }
        let responses: Vec<Value> = req.json()["requests"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| match r["id"].as_str().unwrap() {
                "bad" => json!({ "id": "bad", "status": 404, "body": { "code": "NotFound.File", "message": "gone" } }),
                "slow" => json!({ "id": "slow", "status": 202, "body": { "async_task_id": "task-1" } }),
                id => json!({ "id": id, "status": 204 }),
            })
            .collect();
        Response::json(json!({ "responses": responses }))
    })
    .await;
    let (client, _) = connect(&server).await;

    let res = client.trash(DRIVE_ID, &["ok", "bad", "slow"]).await.unwrap();
    assert_eq!(res.len(), 3);
    assert!(res[0].is_success());
    assert!(res[1].error().unwrap().kind() == ApiErrorKind::NotFound);
    assert_eq!(res[2].async_task_id(), Some("task-1"));

    let req = server.find("/adrive/v4/batch")[0].json();
    assert_eq!(req["resource"], "file");
    assert_eq!(req["requests"][0]["url"], "/recyclebin/trash");
    assert_eq!(
        req["requests"][0]["body"],
        json!({ "drive_id": DRIVE_ID, "file_id": "ok" })
    );

    client.set_starred(DRIVE_ID, &["ok"], true).await.unwrap();
    let star = server.find("/v2/batch")[0].json();
    assert_eq!(star["requests"][0]["method"], "PUT");
    assert_eq!(star["requests"][0]["body"]["custom_index_key"], "starred_yes");

    let ids: Vec<String> = (0..250).map(|i| format!("f{i}")).collect();
    assert_eq!(client.trash(DRIVE_ID, &ids).await.unwrap().len(), 250);
    assert_eq!(server.count("/adrive/v4/batch"), 1 + 3);

    let err = client.purge(DRIVE_ID, &["bad"]).await.unwrap_err();
    assert!(err.is_not_found());
}

#[derive(Default)]
struct Tree {
    /// parent_id -> [(file_id, name, type)]
    children: HashMap<String, Vec<(String, String, String)>>,
    next_id: usize,
}

fn tree_server(auth: Arc<Auth>, tree: Arc<Mutex<Tree>>) -> impl Fn(&Request) -> Response + Send + Sync + 'static {
    move |req| {
        if let Some(resp) = auth.handle(req) {
            return resp;
        }
        if let Some(resp) = auth.check(req) {
            return resp;
        }
        let body = req.json();
        let mut tree = tree.lock().unwrap();
        match req.path.as_str() {
            "/adrive/v3/file/list" => {
                const PAGE: usize = 2;
                let parent = body["parent_file_id"].as_str().unwrap();
                let start: usize = body["marker"].as_str().map(|m| m.parse().unwrap()).unwrap_or(0);
                let all = tree.children.get(parent).cloned().unwrap_or_default();
                let items: Vec<Value> = all
                    .iter()
                    .skip(start)
                    .take(PAGE)
                    .map(|(id, name, kind)| file_json(id, name, kind, parent))
                    .collect();
                let next = if start + PAGE < all.len() {
                    (start + PAGE).to_string()
                } else {
                    String::new()
                };
                Response::json(json!({ "items": items, "next_marker": next }))
            }
            "/adrive/v2/file/createWithFolders" => {
                let parent = body["parent_file_id"].as_str().unwrap().to_owned();
                let name = body["name"].as_str().unwrap().to_owned();
                if tree
                    .children
                    .get(&parent)
                    .is_some_and(|c| c.iter().any(|(_, n, _)| *n == name))
                {
                    return Response::error(409, "AlreadyExist.File");
                }
                tree.next_id += 1;
                let id = format!("new-{}", tree.next_id);
                tree.children
                    .entry(parent.clone())
                    .or_default()
                    .push((id.clone(), name.clone(), "folder".into()));
                Response::json(
                    json!({ "file_id": id, "parent_file_id": parent, "file_name": name, "type": "folder", "drive_id": DRIVE_ID }),
                )
            }
            _ => Response::error(404, "NotFound.View"),
        }
    }
}

fn sample_tree() -> Tree {
    let mut t = Tree::default();
    let e = |id: &str, name: &str, kind: &str| (id.to_owned(), name.to_owned(), kind.to_owned());
    t.children.insert(
        "root".into(),
        vec![
            e("a", "a", "folder"),
            e("r1", "readme.md", "file"),
            e("r2", "x.txt", "file"),
            e("r3", "y.txt", "file"),
        ],
    );
    t.children
        .insert("a".into(), vec![e("b", "b", "folder"), e("a1", "a1.txt", "file")]);
    t.children.insert("b".into(), vec![e("c", "c.txt", "file")]);
    t
}

#[tokio::test]
async fn path_lookup_mkdir_and_walk() {
    let auth = Arc::new(Auth::default());
    let tree = Arc::new(Mutex::new(sample_tree()));
    let server = MockServer::start(tree_server(auth, tree.clone())).await;
    let (client, _) = connect(&server).await;

    assert_eq!(
        client.get_by_path(DRIVE_ID, "/").await.unwrap().unwrap().file_id,
        "root"
    );
    assert_eq!(
        client
            .get_by_path(DRIVE_ID, "/a/b/c.txt")
            .await
            .unwrap()
            .unwrap()
            .file_id,
        "c"
    );
    assert_eq!(
        client.get_by_path(DRIVE_ID, "/y.txt").await.unwrap().unwrap().file_id,
        "r3",
        "needs 2nd page"
    );
    assert!(client.get_by_path(DRIVE_ID, "/a/missing").await.unwrap().is_none());
    assert!(client.get_by_path(DRIVE_ID, "/readme.md/x").await.unwrap().is_none());
    assert!(client.get_by_path(DRIVE_ID, "relative").await.is_err());

    let existing = client.mkdir_p(DRIVE_ID, "/a/b").await.unwrap();
    assert_eq!(existing.file_id, "b");
    assert_eq!(server.count("/adrive/v2/file/createWithFolders"), 0);

    let created = client.mkdir_p(DRIVE_ID, "/a/b/new/deeper").await.unwrap();
    assert!(created.is_folder());
    assert_eq!(created.name, "deeper");
    assert_eq!(server.count("/adrive/v2/file/createWithFolders"), 2);
    assert_eq!(
        client
            .get_by_path(DRIVE_ID, "/a/b/new/deeper")
            .await
            .unwrap()
            .unwrap()
            .file_id,
        created.file_id
    );

    let err = client.mkdir_p(DRIVE_ID, "/readme.md/sub").await.unwrap_err();
    assert!(matches!(err, Error::InvalidInput(_)));

    let mut paths: Vec<String> = client
        .walk(DRIVE_ID, "root")
        .collect()
        .await
        .unwrap()
        .into_iter()
        .map(|e| e.path)
        .collect();
    paths.sort();
    assert_eq!(
        paths,
        [
            "a",
            "a/a1.txt",
            "a/b",
            "a/b/c.txt",
            "a/b/new",
            "a/b/new/deeper",
            "readme.md",
            "x.txt",
            "y.txt"
        ]
    );

    let all = client.list_all_files(DRIVE_ID, "root").await.unwrap();
    assert_eq!(all.len(), 4);
}

#[tokio::test]
async fn cross_drive_requires_distinct_drives() {
    let auth = Arc::new(Auth::default());
    let a = auth.clone();
    let server = MockServer::start(move |req| {
        a.handle(req).unwrap_or_else(|| {
            Response::json(json!({ "items": [{ "drive_id": "res", "file_id": "new", "source_drive_id": DRIVE_ID, "source_file_id": "f1", "status": 201 }] }))
        })
    })
    .await;
    let (client, _) = connect(&server).await;
    assert!(matches!(
        client.cross_drive_copy(DRIVE_ID, &["f1"], DRIVE_ID, "root").await,
        Err(Error::InvalidInput(_))
    ));
    let items = client.cross_drive_copy(DRIVE_ID, &["f1"], "res", "root").await.unwrap();
    assert!(items[0].is_success());
    let body = server.find("/adrive/v2/file/crossDriveCopy")[0].json();
    assert_eq!(body["to_parent_fileId"], "root");
    assert_eq!(body["from_file_ids"], json!(["f1"]));
}

#[tokio::test]
async fn cross_drive_move_targets_backup_drive() {
    let auth = Arc::new(Auth::default());
    let a = auth.clone();
    let server = MockServer::start(move |req| {
        if let Some(resp) = a.handle(req) {
            return resp;
        }
        if let Some(resp) = a.check(req) {
            return resp;
        }
        assert_eq!(req.path, "/adrive/v2/file/crossDriveMove");
        Response::json(json!({ "items": [
            { "drive_id": DRIVE_ID, "file_id": "moved-1", "source_drive_id": "res", "source_file_id": "r1", "status": 201 },
            { "drive_id": DRIVE_ID, "file_id": "", "source_drive_id": "res", "source_file_id": "r2", "status": 400 },
        ] }))
    })
    .await;
    let (client, _) = connect(&server).await;
    assert!(matches!(
        client.cross_drive_move("res", &["r1"], "res", "backup").await,
        Err(Error::InvalidInput(_))
    ));
    assert_eq!(server.count("/adrive/v2/file/crossDriveMove"), 0);

    let items = client
        .cross_drive_move("res", &["r1", "r2"], DRIVE_ID, "backup")
        .await
        .unwrap();
    assert_eq!(items.len(), 2);
    assert!(items[0].is_success());
    assert_eq!(items[0].file_id, "moved-1");
    assert!(!items[1].is_success(), "per-item failures must stay visible");

    let body = server.find("/adrive/v2/file/crossDriveMove")[0].json();
    assert_eq!(
        body,
        json!({
            "from_drive_id": "res",
            "from_file_ids": ["r1", "r2"],
            "to_drive_id": DRIVE_ID,
            "to_parent_fileId": "backup",
        })
    );
    assert_eq!(server.count("/adrive/v2/file/crossDriveCopy"), 0);
}

#[tokio::test]
async fn file_listing_path_and_rename_requests() {
    let auth = Arc::new(Auth::default());
    let a = auth.clone();
    let server = MockServer::start(move |req| {
        if let Some(resp) = a.handle(req) {
            return resp;
        }
        if let Some(resp) = a.check(req) {
            return resp;
        }
        let body = req.json();
        match req.path.as_str() {
            "/adrive/v3/file/list" => match body["marker"].as_str() {
                None => Response::json(json!({
                    "items": [file_json("f1", "a.bin", "file", "root")],
                    "next_marker": "m2",
                })),
                Some("m2") => Response::json(json!({
                    "items": [file_json("f2", "b.bin", "file", "root")],
                    "next_marker": "",
                })),
                Some(other) => panic!("unexpected marker {other}"),
            },
            "/adrive/v1/file/get_path" => Response::json(json!({ "items": [
                file_json("f1", "a.bin", "file", "dir"),
                file_json("dir", "backup", "folder", "root"),
            ] })),
            "/adrive/v3/file/update" => {
                let mut item = file_json(body["file_id"].as_str().unwrap(), "", "file", "root");
                item["name"] = body["name"].clone();
                Response::json(item)
            }
            other => panic!("unexpected path {other}"),
        }
    })
    .await;
    let (client, _) = connect(&server).await;

    let opts = aliyunpan::ListOptions {
        limit: 1,
        ..Default::default()
    };
    let first = client.list_files(DRIVE_ID, "root", &opts).await.unwrap();
    assert_eq!(first.items[0].file_id, "f1");
    assert_eq!(first.next_marker, "m2");
    let second = client
        .list_files(DRIVE_ID, "root", &opts.clone().with_marker(first.next_marker))
        .await
        .unwrap();
    assert_eq!(second.items[0].file_id, "f2");
    assert!(second.next_marker.is_empty());
    let lists = server.find("/adrive/v3/file/list");
    assert_eq!(lists[0].json()["parent_file_id"], "root");
    assert_eq!(lists[0].json()["limit"], 1);
    assert_eq!(lists[0].json()["all"], false);
    assert!(lists[0].json().get("marker").is_none(), "initial page must omit marker");
    assert_eq!(lists[1].json()["marker"], "m2");

    assert_eq!(
        client
            .find_child(DRIVE_ID, "root", "b.bin")
            .await
            .unwrap()
            .unwrap()
            .file_id,
        "f2",
        "find_child must follow pagination"
    );
    assert!(client.find_child(DRIVE_ID, "root", "missing").await.unwrap().is_none());

    let ancestry: Vec<String> = client
        .get_path(DRIVE_ID, "f1")
        .await
        .unwrap()
        .into_iter()
        .map(|i| i.file_id)
        .collect();
    assert_eq!(ancestry, ["f1", "dir"]);
    assert_eq!(
        server.find("/adrive/v1/file/get_path")[0].json(),
        json!({ "drive_id": DRIVE_ID, "file_id": "f1" })
    );

    let renamed = client
        .rename(DRIVE_ID, "f1", "a-2026.bin", CheckNameMode::Refuse)
        .await
        .unwrap();
    assert_eq!(renamed.name, "a-2026.bin");
    assert_eq!(
        server.find("/adrive/v3/file/update")[0].json(),
        json!({ "drive_id": DRIVE_ID, "file_id": "f1", "name": "a-2026.bin", "check_name_mode": "refuse" })
    );
    assert!(matches!(
        client.rename(DRIVE_ID, "f1", "bad/name", CheckNameMode::Refuse).await,
        Err(Error::InvalidInput(_))
    ));
    assert_eq!(server.count("/adrive/v3/file/update"), 1);
}

#[tokio::test]
async fn low_level_upload_requests() {
    let auth = Arc::new(Auth::default());
    let a = auth.clone();
    let server = MockServer::start(move |req| {
        if let Some(resp) = a.handle(req) {
            return resp;
        }
        if let Some(resp) = a.check(req) {
            return resp;
        }
        match req.path.as_str() {
            "/adrive/v2/file/createWithFolders" => Response::json(json!({
                "drive_id": DRIVE_ID,
                "file_id": "up-1",
                "parent_file_id": "root",
                "file_name": "data.bin",
                "type": "file",
                "upload_id": "u-1",
                "rapid_upload": false,
                "part_info_list": [
                    { "part_number": 1, "upload_url": req.url("/part/1") },
                    { "part_number": 2, "upload_url": req.url("/part/2") },
                ],
            })),
            "/v2/file/get_upload_url" => Response::json(json!({
                "drive_id": DRIVE_ID,
                "file_id": "up-1",
                "upload_id": "u-1",
                "part_info_list": [{ "part_number": 2, "upload_url": req.url("/part/2?renewed") }],
            })),
            "/v2/file/list_uploaded_parts" => Response::json(json!({
                "upload_id": "u-1",
                "uploaded_parts": [{ "part_number": 1, "part_size": 4, "etag": "e1" }],
                "next_part_number_marker": "",
            })),
            "/v2/file/complete" => Response::json(file_json("up-1", "data.bin", "file", "root")),
            other => panic!("unexpected path {other}"),
        }
    })
    .await;
    let (client, _) = connect(&server).await;

    let created = client
        .create_upload(&aliyunpan::CreateUpload {
            drive_id: DRIVE_ID,
            parent_file_id: "root",
            name: "data.bin",
            size: 8,
            content_hash: "ABCDEF",
            proof_code: "cHJvb2Y=",
            check_name_mode: CheckNameMode::Overwrite,
            part_count: 2,
        })
        .await
        .unwrap();
    assert_eq!(created.upload_id.as_deref(), Some("u-1"));
    let body = server.find("/adrive/v2/file/createWithFolders")[0].json();
    assert_eq!(
        body["part_info_list"],
        json!([{ "part_number": 1 }, { "part_number": 2 }])
    );
    assert_eq!(body["content_hash"], "ABCDEF");
    assert_eq!(body["content_hash_name"], "sha1");
    assert_eq!(body["proof_code"], "cHJvb2Y=");
    assert_eq!(body["proof_version"], "v1");
    assert_eq!(body["check_name_mode"], "overwrite");
    assert_eq!(body["size"], 8);

    let invalid = aliyunpan::CreateUpload {
        drive_id: DRIVE_ID,
        parent_file_id: "root",
        name: "bad?name",
        size: 0,
        content_hash: "",
        proof_code: "",
        check_name_mode: CheckNameMode::Refuse,
        part_count: 1,
    };
    assert!(matches!(
        client.create_upload(&invalid).await,
        Err(Error::InvalidInput(_))
    ));
    let too_many = aliyunpan::CreateUpload {
        name: "ok.bin",
        part_count: 10_001,
        ..invalid
    };
    assert!(matches!(
        client.create_upload(&too_many).await,
        Err(Error::InvalidInput(_))
    ));
    assert_eq!(server.count("/adrive/v2/file/createWithFolders"), 1);

    let urls = client.get_upload_url(DRIVE_ID, "up-1", "u-1", &[2]).await.unwrap();
    assert_eq!(urls.part_info_list[0].part_number, 2);
    assert!(urls.part_info_list[0].upload_url.ends_with("/part/2?renewed"));
    assert_eq!(
        server.find("/v2/file/get_upload_url")[0].json(),
        json!({ "drive_id": DRIVE_ID, "file_id": "up-1", "upload_id": "u-1", "part_info_list": [{ "part_number": 2 }] })
    );

    let parts = client.list_uploaded_parts(DRIVE_ID, "up-1", "u-1", None).await.unwrap();
    assert_eq!(parts.uploaded_parts[0].part_number, 1);
    assert_eq!(parts.uploaded_parts[0].part_size, 4);
    client
        .list_uploaded_parts(DRIVE_ID, "up-1", "u-1", Some(7))
        .await
        .unwrap();
    let listed = server.find("/v2/file/list_uploaded_parts");
    assert!(
        listed[0].json().get("part_number_marker").is_none(),
        "initial marker must be omitted"
    );
    assert_eq!(listed[1].json()["part_number_marker"], 7);

    let done = client.complete_upload(DRIVE_ID, "up-1", "u-1").await.unwrap();
    assert_eq!(done.file_id, "up-1");
    assert_eq!(
        server.find("/v2/file/complete")[0].json(),
        json!({ "ignoreError": true, "drive_id": DRIVE_ID, "file_id": "up-1", "upload_id": "u-1" })
    );
}

#[tokio::test]
async fn download_url_and_video_preview_requests() {
    let auth = Arc::new(Auth::default());
    let a = auth.clone();
    let server = MockServer::start(move |req| {
        if let Some(resp) = a.handle(req) {
            return resp;
        }
        if req.path == "/dl/f1" {
            assert!(
                req.header("authorization").is_none(),
                "presigned URLs must not carry tokens"
            );
            return Response::bytes(200, b"data".to_vec());
        }
        if let Some(resp) = a.check(req) {
            return resp;
        }
        match req.path.as_str() {
            "/v2/file/get_download_url" => Response::json(json!({
                "method": "GET",
                "url": req.url("/dl/f1"),
                "expiration": "2026-10-07T12:00:00.000Z",
                "size": 4,
            })),
            "/v2/file/get_video_preview_play_info" => Response::json(json!({
                "drive_id": DRIVE_ID,
                "file_id": "v1",
                "video_preview_play_info": {
                    "category": "live_transcoding",
                    "meta": { "duration": 2.0 },
                    "live_transcoding_task_list": [
                        { "template_id": "LD", "status": "finished", "url": "https://example.invalid/ld.m3u8" },
                    ],
                },
            })),
            other => panic!("unexpected path {other}"),
        }
    })
    .await;
    let (client, _) = connect(&server).await;

    let url = client.get_download_url(DRIVE_ID, "f1", 600).await.unwrap();
    assert_eq!(url.size, 4);
    assert_eq!(url.expiration.as_deref(), Some("2026-10-07T12:00:00.000Z"));
    assert_eq!(
        server.find("/v2/file/get_download_url")[0].json(),
        json!({ "drive_id": DRIVE_ID, "file_id": "f1", "expire_sec": 600 })
    );
    let data = client
        .open_download_url(&url.url, None)
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    assert_eq!(data, "data");

    let info = client.get_video_preview_play_info(DRIVE_ID, "v1").await.unwrap();
    let tasks = &info.video_preview_play_info.live_transcoding_task_list;
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].status.as_deref(), Some("finished"));
    assert_eq!(
        server.find("/v2/file/get_video_preview_play_info")[0].json(),
        json!({ "category": "live_transcoding", "drive_id": DRIVE_ID, "file_id": "v1", "template_id": "" })
    );
}

#[tokio::test]
async fn share_helpers_paginate_and_parse_id() {
    let auth = Arc::new(Auth::default());
    let a = auth.clone();
    let server = MockServer::start(move |req| {
        if let Some(resp) = a.handle(req) {
            return resp;
        }
        if let Some(resp) = a.check(req) {
            return resp;
        }
        match req.path.as_str() {
            "/adrive/v3/share_link/list" => {
                let body = req.json();
                let marker = body["marker"].as_str().unwrap_or_default();
                if marker.is_empty() {
                    Response::json(json!({
                        "items": [{"share_id": "s1", "share_name": "one"}],
                        "next_marker": "m2"
                    }))
                } else {
                    Response::json(json!({
                        "items": [{"share_id": "s2", "share_name": "two"}],
                        "next_marker": ""
                    }))
                }
            }
            "/v2/share_link/get_share_token" => Response::json(json!({ "share_token": "tok", "expires_in": 7200 })),
            _ => Response::error(404, "NotFound.View"),
        }
    })
    .await;
    let (client, _) = connect(&server).await;

    let links = client
        .list_all_share_links(&ListOptions {
            limit: 1,
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(links.len(), 2);
    assert_eq!(links[0].share_id, "s1");
    assert_eq!(links[1].share_id, "s2");

    let token = client
        .get_share_token_by_url("https://www.alipan.com/s/AbC123?pwd=0000", "0000")
        .await
        .unwrap();
    assert_eq!(token.share_token, "tok");
    assert_eq!(
        server.find("/v2/share_link/get_share_token")[0].json()["share_id"],
        "AbC123"
    );

    assert_eq!(
        Client::extract_share_id("https://www.alipan.com/?share_id=xyz-7&foo=1"),
        Some("xyz-7".into())
    );
}

#[tokio::test]
async fn uploaded_parts_paginates() {
    let auth = Arc::new(Auth::default());
    let a = auth.clone();
    let server = MockServer::start(move |req| {
        if let Some(resp) = a.handle(req) {
            return resp;
        }
        if let Some(resp) = a.check(req) {
            return resp;
        }
        if req.path == "/v2/file/list_uploaded_parts" {
            let body = req.json();
            let marker = body.get("part_number_marker").and_then(Value::as_u64);
            return match marker {
                None => Response::json(json!({
                    "uploaded_parts": [
                        {"part_number": 1, "part_size": 4},
                        {"part_number": 2, "part_size": 4}
                    ],
                    "next_part_number_marker": "3"
                })),
                Some(3) => Response::json(json!({
                    "uploaded_parts": [{"part_number": 3, "part_size": 2}],
                    "next_part_number_marker": ""
                })),
                _ => Response::error(400, "BadRequest"),
            };
        }
        Response::error(404, "NotFound.View")
    })
    .await;
    let (client, _) = connect(&server).await;

    let parts = client
        .list_all_uploaded_parts(DRIVE_ID, "file", "upload")
        .await
        .unwrap();
    assert_eq!(parts.iter().map(|p| p.part_number).collect::<Vec<_>>(), vec![1, 2, 3]);

    let calls = server.find("/v2/file/list_uploaded_parts");
    assert_eq!(calls.len(), 2);
    assert!(calls[0].json().get("part_number_marker").is_none());
    assert_eq!(calls[1].json()["part_number_marker"], 3);
}

#[tokio::test]
async fn recycle_and_album_helpers_paginate_and_wait() {
    let auth = Arc::new(Auth::default());
    let a = auth.clone();
    let task_hits = Arc::new(AtomicUsize::new(0));
    let h = task_hits.clone();
    let server = MockServer::start(move |req| {
        if let Some(resp) = a.handle(req) {
            return resp;
        }
        if let Some(resp) = a.check(req) {
            return resp;
        }
        match req.path.as_str() {
            "/adrive/v2/recyclebin/list" => {
                let body = req.json();
                let marker = body["marker"].as_str().unwrap_or_default();
                if marker.is_empty() {
                    Response::json(json!({
                        "items": [file_json("r1", "a.txt", "file", "root")],
                        "next_marker": "m2"
                    }))
                } else {
                    Response::json(json!({
                        "items": [file_json("r2", "b.txt", "file", "root")],
                        "next_marker": ""
                    }))
                }
            }
            "/v2/recyclebin/clear" => Response::json(json!({ "async_task_id": "task-1" })),
            "/v2/async_task/get" => {
                if h.fetch_add(1, Ordering::SeqCst) == 0 {
                    Response::json(json!({ "async_task_id": "task-1", "state": "Running" }))
                } else {
                    Response::json(json!({ "async_task_id": "task-1", "state": "Succeed" }))
                }
            }
            "/adrive/v1/album/list" => {
                let body = req.json();
                let marker = body["marker"].as_str().unwrap_or_default();
                if marker.is_empty() {
                    Response::json(json!({
                        "items": [{"album_id":"a1","name":"one"}],
                        "next_marker": "m2"
                    }))
                } else {
                    Response::json(json!({
                        "items": [{"album_id":"a2","name":"two"}],
                        "next_marker": ""
                    }))
                }
            }
            "/adrive/v1/album/list_files" => {
                let body = req.json();
                let marker = body["marker"].as_str().unwrap_or_default();
                if marker.is_empty() {
                    Response::json(json!({
                        "items": [file_json("f1", "x.jpg", "file", "root")],
                        "next_marker": "m2"
                    }))
                } else {
                    Response::json(json!({
                        "items": [file_json("f2", "y.jpg", "file", "root")],
                        "next_marker": ""
                    }))
                }
            }
            _ => Response::error(404, "NotFound.View"),
        }
    })
    .await;
    let (client, _) = connect(&server).await;

    let recycle = client
        .list_all_recycle_bin(
            DRIVE_ID,
            &ListOptions {
                limit: 1,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(recycle.len(), 2);

    let task = client
        .clear_recycle_bin_and_wait(DRIVE_ID, Duration::from_millis(1), Duration::from_secs(1))
        .await
        .unwrap();
    assert!(task.is_succeed());
    assert_eq!(task_hits.load(Ordering::SeqCst), 2);

    let albums = client
        .list_all_albums(&ListOptions {
            limit: 1,
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(albums.len(), 2);

    let album_files = client
        .list_all_album_files(
            "a1",
            &ListOptions {
                limit: 1,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(album_files.len(), 2);
}
