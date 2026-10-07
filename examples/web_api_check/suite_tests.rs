use super::*;
use crate::common::{self, Auth, MockServer, Response, file_json};
use aliyunpan::{ApiError, FileKind};
use std::sync::{Arc, Mutex};

#[test]
fn catalog_covers_every_public_async_api_and_helper() {
    let sources = [
        include_str!("../../src/api/user.rs"),
        include_str!("../../src/api/file.rs"),
        include_str!("../../src/api/batch.rs"),
        include_str!("../../src/api/recycle.rs"),
        include_str!("../../src/api/share.rs"),
        include_str!("../../src/api/album.rs"),
        include_str!("../../src/upload.rs"),
        include_str!("../../src/download.rs"),
        include_str!("../../src/path.rs"),
        include_str!("../../src/client.rs"),
    ];
    let local_or_stream = [
        "credentials",
        "user_id",
        "default_drive_id",
        "access_token",
        "chunk",
        "bytes",
        "next",
        "collect",
    ];
    for source in sources {
        for signature in source.split("pub async fn ").skip(1) {
            let name = signature.split(['(', '<']).next().unwrap();
            assert!(
                METHODS.contains(&name) || local_or_stream.contains(&name),
                "untracked API: {name}"
            );
        }
    }
    let unique: std::collections::BTreeSet<_> = METHODS.iter().collect();
    assert_eq!(unique.len(), METHODS.len());
}

#[test]
fn rejects_empty_partial_duplicate_and_failed_batches() {
    assert!(batch_ok(vec![], 1).is_err());
    let ok = BatchResponse {
        id: "f".into(),
        status: 204,
        body: json!({}),
    };
    assert!(batch_ok(vec![ok.clone()], 2).is_err());
    assert!(batch_ok(vec![ok.clone(), ok.clone()], 2).is_err());
    assert!(
        batch_ok(
            vec![BatchResponse {
                status: 400,
                ..ok.clone()
            }],
            1
        )
        .is_err()
    );
    assert!(
        batch_ok(
            vec![BatchResponse {
                body: json!({"code": "PermissionDenied"}),
                ..ok.clone()
            }],
            1
        )
        .is_err()
    );
    assert!(batch_ok(vec![ok], 1).is_ok());
}

#[tokio::test]
async fn failures_remain_failed_and_sensitive_bodies_never_enter_reports() {
    let mut report = Report::new();
    let result: Result<()> = report
        .case("get_file", async {
            Err(Error::Api(ApiError {
                code: "PRIVATE-ERROR".into(),
                message: "SECRET-CONTENT".into(),
                display_message: Some("SECRET-CONTENT".into()),
                http_status: 403,
            }))
        })
        .await;
    assert!(result.is_err());
    report.case("get_file", async { Ok(()) }).await.unwrap();
    assert_eq!(report.outcomes["get_file"].status, Status::Failed);
    assert_eq!(report.outcomes["download"].status, Status::Blocked);
    assert_eq!(report.outcomes["device_logout"].status, Status::Excluded);
    let text = serde_json::to_string(&report.outcomes).unwrap();
    assert!(!text.contains("SECRET-CONTENT"));
    assert!(!text.contains("PRIVATE-ERROR"));
    assert!(report.failed());
}

fn sandbox(id: &str) -> Sandbox {
    Sandbox {
        drive: common::DRIVE_ID.into(),
        id: id.into(),
        name: "aliyunpan-sdk-check-unit-test".into(),
    }
}

fn metadata(root: &Sandbox) -> FileItem {
    FileItem {
        drive_id: root.drive.clone(),
        file_id: root.id.clone(),
        name: root.name.clone(),
        parent_file_id: "root".into(),
        kind: FileKind::Folder,
        ..Default::default()
    }
}

#[test]
fn cleanup_ownership_requires_exact_generated_root() {
    let root = sandbox("owned");
    let file = metadata(&root);
    assert!(owns_root(&root, &file));
    for different in [
        FileItem {
            drive_id: "other-drive".into(),
            ..file.clone()
        },
        FileItem {
            name: "user-folder".into(),
            ..file.clone()
        },
        FileItem {
            file_id: "root".into(),
            ..file.clone()
        },
        FileItem {
            parent_file_id: "other-parent".into(),
            ..file.clone()
        },
        FileItem {
            kind: FileKind::File,
            ..file.clone()
        },
    ] {
        assert!(!owns_root(&root, &different));
    }
    assert!(!owns_root(&sandbox("root"), &metadata(&sandbox("root"))));
}

#[tokio::test]
async fn cleanup_refuses_foreign_folder_and_continues_after_share_failure() {
    let auth = Auth::default();
    let server = MockServer::start(move |req| {
        if let Some(response) = auth.handle(req) {
            return response;
        }
        match req.path.as_str() {
            "/adrive/v4/batch" => Response::json(json!({
                "responses": [{"id": "test-share", "status": 403, "body": {"code": "PermissionDenied"}}]
            })),
            "/v2/file/get" => Response::json(file_json("foreign", "user-folder", "folder", "root")),
            _ => Response::error(404, "UnexpectedRequest"),
        }
    })
    .await;
    let (client, _) = common::connect(&server).await;
    let mut resources = Resources {
        roots: vec![sandbox("foreign")],
        shares: vec!["test-share".into()],
        ..Default::default()
    };
    let mut report = Report::new();
    resources.cleanup(&client, &mut report).await;
    assert_eq!(report.outcomes["cleanup/shares"].status, Status::Failed);
    assert_eq!(report.outcomes["cleanup/sandboxes"].status, Status::Failed);
    assert_eq!(server.count("/v2/file/get"), 1);
    let batches = server.find("/adrive/v4/batch");
    assert_eq!(batches.len(), 1);
    assert_eq!(batches[0].json()["requests"][0]["url"], "/share_link/cancel");
}

#[tokio::test]
async fn cleanup_deletes_only_owned_root_and_verifies_disappearance() {
    let auth = Auth::default();
    let deleted = Arc::new(Mutex::new(false));
    let state = deleted.clone();
    let server = MockServer::start(move |req| {
        if let Some(response) = auth.handle(req) {
            return response;
        }
        match req.path.as_str() {
            "/v2/file/get" if *state.lock().unwrap() => Response::error(404, "NotFound.File"),
            "/v2/file/get" => Response::json(file_json("owned", "aliyunpan-sdk-check-unit-test", "folder", "root")),
            "/adrive/v4/batch" => {
                let body = req.json();
                let r = &body["requests"][0];
                assert_eq!(r["body"]["file_id"], "owned");
                assert_eq!(r["body"]["drive_id"], common::DRIVE_ID);
                if r["url"] == "/file/delete" {
                    *state.lock().unwrap() = true;
                }
                Response::json(json!({"responses": [{"id": "owned", "status": 204}]}))
            }
            _ => Response::error(404, "UnexpectedRequest"),
        }
    })
    .await;
    let (client, _) = common::connect(&server).await;
    let mut resources = Resources {
        roots: vec![sandbox("owned")],
        ..Default::default()
    };
    let mut report = Report::new();
    resources.cleanup(&client, &mut report).await;
    assert!(!report.failed());
    assert!(*deleted.lock().unwrap());
    assert_eq!(report.outcomes["purge"].status, Status::Passed);
    assert_eq!(server.count("/adrive/v4/batch"), 2);
}

#[tokio::test]
async fn independent_account_cases_continue_after_rejection() {
    let auth = Auth::default();
    let server = MockServer::start(move |req| {
        if let Some(response) = auth.handle(req) {
            return response;
        }
        match req.path.as_str() {
            "/v2/user/get" => Response::error(403, "PermissionDenied"),
            "/v2/databox/get_personal_info" => Response::json(json!({"personal_space_info": {"total_size": 1024}})),
            "/v2/sbox/get" => Response::json(json!({"insurance_enabled": false})),
            "/adrive/v1/user/albums_info" => Response::json(json!({"data": {"driveId": "album-drive"}})),
            "/business/v1.0/users/vip/info" => Response::json(json!({"identity": "member"})),
            "/users/v1/users/device/renew_session" => Response::json(json!({"result": true, "success": true})),
            "/adrive/v3/share_link/list" | "/adrive/v1/album/list" | "/adrive/v3/file/list" => {
                Response::json(json!({"items": []}))
            }
            _ => Response::error(404, "UnexpectedRequest"),
        }
    })
    .await;
    let (client, _) = common::connect(&server).await;
    let mut report = Report::new();
    account(&client, &mut report).await;
    assert_eq!(report.outcomes["get_user_info"].status, Status::Failed);
    for case in [
        "renew_session",
        "get_personal_info",
        "get_sbox_info",
        "get_albums_info",
        "get_vip_info",
        "list_albums",
        "list_all_albums",
    ] {
        assert_eq!(report.outcomes[case].status, Status::Passed, "{case}");
    }
}

#[tokio::test]
async fn known_sharing_blockers_do_not_fail_their_parent_group() {
    let mut report = Report::new();
    let error = Error::UnexpectedResponse {
        operation: "create_share_link",
        shape: "root=object; share_id=missing; unknown_fields=2".into(),
    };
    let result: Result<()> = report.case("create_share_link", async { Err(error) }).await;
    report.group("group/shares", result);
    assert_eq!(report.outcomes["create_share_link"].status, Status::Blocked);
    assert_eq!(report.outcomes["group/shares"].status, Status::Blocked);
    assert!(!report.failed());
    report.case("create_share_link", async { Ok(()) }).await.unwrap();
    assert_eq!(report.outcomes["create_share_link"].status, Status::Passed);
}

#[tokio::test]
async fn blocker_policy_does_not_hide_transport_unknown_or_cleanup_errors() {
    for name in ["create_share_link", "renew_session", "cancel_share_links", "purge"] {
        let mut report = Report::new();
        let result: Result<()> = report
            .case(name, async {
                Err(Error::Http {
                    status: 500,
                    body: "private".into(),
                })
            })
            .await;
        assert!(result.is_err());
        assert_eq!(report.outcomes[name].status, Status::Failed);
        assert!(report.failed());
    }
    let mut report = Report::new();
    let error = Error::UnexpectedResponse {
        operation: "create_share_link",
        shape: "share_id=missing".into(),
    };
    let result: Result<()> = report.case("cancel_share_links", async { Err(error) }).await;
    report.group("cleanup/shares", result);
    assert_eq!(report.outcomes["cleanup/shares"].status, Status::Failed);
}

#[tokio::test]
async fn renewal_signature_blocker_requires_successful_post_recovery_check() {
    for healthy_after in [true, false] {
        let auth = Arc::new(Auth::default());
        let handler = auth.clone();
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let server = MockServer::start(move |req| {
            if let Some(response) = handler.handle(req) {
                return response;
            }
            if req.path == "/users/v1/users/device/renew_session" {
                return Response::error(400, "DeviceSessionSignatureInvalid");
            }
            if req.path == "/adrive/v3/file/list" {
                if calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) > 0 && !healthy_after {
                    return Response::error(403, "PermissionDenied");
                }
                return handler
                    .check(req)
                    .unwrap_or_else(|| Response::json(json!({"items": []})));
            }
            Response::error(404, "UnexpectedRequest")
        })
        .await;
        let (client, _) = common::connect(&server).await;
        let mut report = Report::new();
        session_renewal(&client, &mut report).await;
        assert_eq!(report.outcomes["renew_session"].status, Status::Blocked);
        assert_eq!(report.outcomes["session/before-renew"].status, Status::Passed);
        assert_eq!(
            report.outcomes["session/after-renew"].status,
            if healthy_after { Status::Passed } else { Status::Failed }
        );
        assert_eq!(report.failed(), !healthy_after);
        assert_eq!(auth.sessions.load(std::sync::atomic::Ordering::SeqCst), 2);
        assert_eq!(server.count("/users/v1/users/device/renew_session"), 1);
    }
}

#[tokio::test]
async fn multipart_exercises_resume_and_direct_empty_upload() {
    exercise_multipart(false).await;
}

#[tokio::test]
async fn rapid_empty_upload_with_upload_id_never_requests_a_pending_session() {
    exercise_multipart(true).await;
}

async fn exercise_multipart(rapid_empty: bool) {
    let auth = Auth::default();
    let current = Mutex::new(json!({}));
    let server = MockServer::start(move |req| {
        if let Some(response) = auth.handle(req) {
            return response;
        }
        match req.path.as_str() {
            "/adrive/v2/file/createWithFolders" => {
                *current.lock().unwrap() = req.json();
                Response::json(json!({
                    "file_id": "uploaded", "upload_id": "upload-1",
                    "rapid_upload": rapid_empty && req.json()["size"] == 0,
                    "part_info_list": [
                        {"part_number": 1, "upload_url": req.url("/put/1")},
                        {"part_number": 2, "upload_url": req.url("/put/2")}
                    ]
                }))
            }
            "/v2/file/get_upload_url" => {
                if rapid_empty && current.lock().unwrap()["size"] == 0 {
                    return Response::error(404, "NotFound.UploadId");
                }
                let number = req.json()["part_info_list"][0]["part_number"].as_u64().unwrap();
                Response::json(json!({
                    "part_info_list": [{"part_number": number, "upload_url": req.url(&format!("/put/{number}"))}]
                }))
            }
            "/put/1" | "/put/2" => Response::bytes(200, vec![]),
            "/v2/file/list_uploaded_parts" => Response::json(json!({
                "uploaded_parts": [{"part_number": 1, "part_size": 524288}], "next_part_number_marker": ""
            })),
            "/v2/file/complete" | "/v2/file/get" => {
                let source = current.lock().unwrap();
                if req.path == "/v2/file/complete" && rapid_empty && source["size"] == 0 {
                    return Response::error(404, "NotFound.UploadId");
                }
                Response::json(json!({
                    "file_id": "uploaded", "drive_id": common::DRIVE_ID, "type": "file",
                    "size": source["size"], "content_hash": source["content_hash"]
                }))
            }
            _ => Response::error(404, "UnexpectedRequest"),
        }
    })
    .await;
    let (client, _) = common::connect(&server).await;
    let mut report = Report::new();
    multipart(&client, &mut report, &sandbox("owned")).await.unwrap();
    for name in [
        "begin_upload",
        "get_upload_url",
        "upload_part",
        "list_uploaded_parts",
        "list_all_uploaded_parts",
        "resume_upload",
        "create_upload",
        "complete_upload",
    ] {
        assert_eq!(report.outcomes[name].status, Status::Passed, "{name}");
    }
    assert_eq!(server.count("/put/1"), if rapid_empty { 1 } else { 2 });
    if !rapid_empty {
        assert_eq!(server.find("/put/1")[1].body.len(), 0);
    }
    assert_eq!(server.count("/put/2"), 1);
    assert_eq!(server.count("/v2/file/complete"), if rapid_empty { 1 } else { 2 });
}

#[test]
fn share_expiration_is_a_bounded_utc_timestamp() {
    let timestamp = share_expiration().unwrap();
    assert_eq!(timestamp.len(), 20);
    assert!(timestamp.ends_with('Z'));
    assert_eq!(&timestamp[10..11], "T");
}

#[tokio::test]
async fn missing_resource_drive_is_blocked_and_never_mutates() {
    let auth = Auth::default();
    let server = MockServer::start(move |req| {
        auth.handle(req)
            .unwrap_or_else(|| Response::json(json!({"resource_drive_id": ""})))
    })
    .await;
    let (client, _) = common::connect(&server).await;
    let mut report = Report::new();
    let mut resources = Resources::default();
    cross_drive(&client, &mut report, &mut resources, &sandbox("owned"))
        .await
        .unwrap();
    assert_eq!(report.outcomes["cross_drive_copy"].status, Status::Blocked);
    assert_eq!(report.outcomes["cross_drive_move"].status, Status::Blocked);
    assert!(resources.roots.is_empty());
    assert_eq!(server.count("/adrive/v2/file/createWithFolders"), 0);
}

#[tokio::test]
async fn async_task_failure_is_not_a_successful_mutation() {
    let auth = Auth::default();
    let server = MockServer::start(move |req| {
        if let Some(response) = auth.handle(req) {
            return response;
        }
        match req.path.as_str() {
            "/v2/async_task/get" => Response::json(json!({"async_task_id": "task-1", "state": "Failed"})),
            "/adrive/v2/batch" => Response::json(json!({
                "responses": [{"id": "task-1", "status": 200, "body": {"async_task_id": "task-1", "state": "Failed"}}]
            })),
            _ => Response::error(404, "UnexpectedRequest"),
        }
    })
    .await;
    let (client, _) = common::connect(&server).await;
    let mut report = Report::new();
    let pending = BatchResponse {
        id: "file-1".into(),
        status: 202,
        body: json!({"async_task_id": "task-1"}),
    };
    assert!(tasks(&client, &mut report, &[pending]).await.is_err());
    assert_eq!(report.outcomes["get_async_task"].status, Status::Passed);
    assert_eq!(report.outcomes["get_async_tasks"].status, Status::Passed);
    assert_eq!(report.outcomes["wait_async_task"].status, Status::Failed);
}

#[tokio::test]
async fn connection_failure_still_writes_complete_redacted_inventory() {
    let id = Credentials::from_refresh_token("test").unwrap().device_id;
    let directory = std::env::temp_dir().join(format!("aliyunpan-report-test-{id}"));
    fs::create_dir(&directory).unwrap();
    let result = run(Config::default(), aliyunpan::MemoryStore::default(), &directory).await;
    assert!(result.is_err());
    let report: serde_json::Value =
        serde_json::from_slice(&fs::read(directory.join("api-report.json")).unwrap()).unwrap();
    assert_eq!(report.as_object().unwrap().len(), METHODS.len());
    assert_eq!(report["connect"]["status"], "FAILED");
    assert_eq!(report["upload"]["status"], "BLOCKED");
    assert_eq!(report["device_logout"]["status"], "EXCLUDED");
    assert!(
        fs::read_to_string(directory.join("api-report.md"))
            .unwrap()
            .contains("| upload |")
    );
    fs::remove_file(directory.join("api-report.json")).unwrap();
    fs::remove_file(directory.join("api-report.md")).unwrap();
    fs::remove_dir(directory).unwrap();
}
