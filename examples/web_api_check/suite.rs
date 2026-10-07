//! Isolated live API contract tests. Reports contain no raw responses or credentials.

use std::{collections::BTreeMap, fs, future::Future, path::Path, time::Duration};

use aliyunpan::{
    BatchRequest, BatchResponse, BatchVersion, Bytes, CheckNameMode, Client, Config, CreateUpload, Credentials,
    DriveFile, Error, FileItem, ListOptions, OrderBy, Result, TokenStore, UploadOptions, UploadSource, UploadStart,
    UploadState,
};
use serde::Serialize;
use serde_json::json;
use sha1::{Digest, Sha1};

use super::checked;

const METHODS: &[&str] = &[
    "connect",
    "connect_shared",
    "force_refresh",
    "recreate_session",
    "renew_session",
    "get_user_info",
    "get_personal_info",
    "get_sbox_info",
    "get_albums_info",
    "get_vip_info",
    "list_files",
    "list_all_files",
    "get_file",
    "get_path",
    "create_folder",
    "rename",
    "find_child",
    "get_by_path",
    "mkdir_p",
    "walk",
    "create_upload",
    "get_upload_url",
    "upload_part",
    "complete_upload",
    "list_uploaded_parts",
    "list_all_uploaded_parts",
    "begin_upload",
    "resume_upload",
    "upload",
    "upload_file",
    "get_download_url",
    "open_download_url",
    "download",
    "download_to_file",
    "batch",
    "move_files",
    "set_starred",
    "star_files",
    "unstar_files",
    "trash",
    "restore",
    "delete_permanently",
    "purge",
    "list_recycle_bin",
    "list_all_recycle_bin",
    "get_async_task",
    "get_async_tasks",
    "wait_async_task",
    "cross_drive_copy",
    "cross_drive_move",
    "create_share_link",
    "list_share_links",
    "list_all_share_links",
    "get_share_by_anonymous",
    "get_share_token",
    "get_share_token_by_url",
    "list_shared_files",
    "list_all_shared_files",
    "save_from_share",
    "cancel_share_links",
    "create_fast_share",
    "create_fast_share_from_ids",
    "list_albums",
    "list_all_albums",
    "create_album",
    "update_album",
    "get_album",
    "delete_album",
    "list_album_files",
    "list_all_album_files",
    "add_album_files",
    "delete_album_files",
    "create_album_share",
    "get_video_preview_play_info",
    "clear_recycle_bin",
    "clear_recycle_bin_and_wait",
    "device_logout",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
enum Status {
    Passed,
    Failed,
    Blocked,
    Excluded,
}

#[derive(Serialize)]
struct Outcome {
    status: Status,
    detail: String,
}

struct Report {
    outcomes: BTreeMap<String, Outcome>,
}

impl Report {
    fn new() -> Self {
        let mut report = Self {
            outcomes: METHODS
                .iter()
                .map(|name| {
                    (
                        (*name).to_owned(),
                        Outcome {
                            status: Status::Blocked,
                            detail: "prerequisite did not complete".into(),
                        },
                    )
                })
                .collect(),
        };
        for name in ["clear_recycle_bin", "clear_recycle_bin_and_wait"] {
            report.set(
                name,
                Status::Excluded,
                "would permanently delete unrelated recycle-bin contents",
            );
        }
        report.set(
            "device_logout",
            Status::Excluded,
            "could invalidate the scheduled job's credentials",
        );
        for name in ["get_async_task", "get_async_tasks", "wait_async_task"] {
            report.set(
                name,
                Status::Blocked,
                "requires a real task ID returned by a test mutation",
            );
        }
        report
    }

    fn set(&mut self, name: &str, status: Status, detail: &str) {
        // A later successful cleanup/retry must not erase a failed contract check.
        if self.outcomes.get(name).is_some_and(|o| o.status == Status::Failed) {
            return;
        }
        self.outcomes.insert(
            name.into(),
            Outcome {
                status,
                detail: detail.into(),
            },
        );
    }

    async fn case<T>(&mut self, name: &str, future: impl Future<Output = Result<T>>) -> Result<T> {
        let result = future.await;
        match &result {
            Ok(_) => self.set(name, Status::Passed, "response and case assertions passed"),
            Err(_) => {
                let error = result.err().expect("matched an error");
                let detail = checked::<()>(name, Err(error)).unwrap_err();
                self.set(name, Status::Failed, &detail);
                return Err(Error::InvalidInput(format!("case {name} failed")));
            }
        }
        result
    }

    fn group(&mut self, name: &str, result: Result<()>) {
        if let Err(error) = result {
            let detail = checked::<()>(name, Err(error)).unwrap_err();
            self.set(name, Status::Failed, &detail);
        }
    }

    fn write(&self, directory: &Path) -> std::result::Result<(), String> {
        let data = serde_json::to_vec_pretty(&self.outcomes).map_err(|_| "serialize API report failed")?;
        fs::write(directory.join("api-report.json"), data).map_err(|_| "write API JSON report failed")?;
        let mut text = String::from("# SDK live API contract tests\n\n");
        text.push_str("PASSED = executed and checked; FAILED = request/assertion failed; BLOCKED = not verified; EXCLUDED = safety policy.\n\n");
        for status in [Status::Passed, Status::Failed, Status::Blocked, Status::Excluded] {
            let count = self.outcomes.values().filter(|o| o.status == status).count();
            text.push_str(&format!("{status:?}: {count}  \n"));
        }
        text.push('\n');
        text.push_str("| API / case | Result | Detail |\n| --- | --- | --- |\n");
        for (name, outcome) in &self.outcomes {
            text.push_str(&format!("| {name} | {:?} | {} |\n", outcome.status, outcome.detail));
        }
        fs::write(directory.join("api-report.md"), text).map_err(|_| "write API Markdown report failed")?;
        Ok(())
    }

    fn failed(&self) -> bool {
        self.outcomes.values().any(|o| o.status == Status::Failed)
    }
}

fn verify(condition: bool, message: &'static str) -> Result<()> {
    if condition {
        Ok(())
    } else {
        eprintln!("API contract assertion failed: {message}");
        Err(Error::InvalidInput(message.into()))
    }
}

fn batch_ok(responses: Vec<BatchResponse>, expected: usize) -> Result<Vec<BatchResponse>> {
    verify(responses.len() == expected, "batch response count mismatch")?;
    let ids: std::collections::BTreeSet<_> = responses.iter().map(|r| &r.id).collect();
    verify(
        ids.len() == expected && ids.iter().all(|id| !id.is_empty()),
        "batch response IDs missing or duplicated",
    )?;
    responses.into_iter().map(BatchResponse::into_result).collect()
}

fn sha1(data: &[u8]) -> String {
    Sha1::digest(data).iter().map(|b| format!("{b:02X}")).collect()
}

fn options() -> UploadOptions {
    UploadOptions {
        check_name_mode: CheckNameMode::Refuse,
        chunk_size: Some(512 * 1024),
    }
}

fn page_options() -> ListOptions {
    ListOptions {
        limit: 1,
        ..Default::default()
    }
}

#[derive(Clone)]
struct Sandbox {
    drive: String,
    id: String,
    name: String,
}

fn owns_root(root: &Sandbox, file: &FileItem) -> bool {
    root.name.starts_with("aliyunpan-sdk-check-")
        && !root.id.is_empty()
        && root.id != "root"
        && file.file_id == root.id
        && file.drive_id == root.drive
        && file.name == root.name
        && file.parent_file_id == "root"
        && file.is_folder()
}

#[derive(Default)]
struct Resources {
    roots: Vec<Sandbox>,
    albums: Vec<String>,
    shares: Vec<String>,
    trashed: Vec<DriveFile>,
    album_files: Vec<(String, Vec<DriveFile>)>,
}

impl Resources {
    async fn root(&mut self, client: &Client, drive: &str, name: &str) -> Result<Sandbox> {
        let created = client.create_folder(drive, "root", name, CheckNameMode::Refuse).await?;
        verify(
            !created.exist && !created.file_id.is_empty() && created.file_id != "root",
            "invalid new sandbox",
        )?;
        let root = Sandbox {
            drive: drive.into(),
            id: created.file_id,
            name: name.into(),
        };
        self.roots.push(root.clone());
        let meta = client.get_file(drive, &root.id).await?;
        verify(owns_root(&root, &meta), "sandbox ownership check failed")?;
        Ok(root)
    }

    async fn cleanup(&mut self, client: &Client, report: &mut Report) {
        for id in &self.shares {
            let result = report
                .case("cancel_share_links", async {
                    batch_ok(client.cancel_share_links(&[id]).await?, 1)?;
                    let active = client.list_all_share_links(&ListOptions::default()).await?;
                    verify(
                        !active.iter().any(|share| &share.share_id == id),
                        "canceled share still active",
                    )
                })
                .await;
            report.group("cleanup/shares", result);
        }
        for (album, files) in &self.album_files {
            let result = report
                .case("delete_album_files", client.delete_album_files(album, files))
                .await;
            report.group("cleanup/album-members", result);
        }
        for id in &self.albums {
            let result = report
                .case("delete_album", async {
                    client.delete_album(id).await?;
                    let albums = client
                        .list_all_albums(&ListOptions {
                            order_by: OrderBy::CreatedAt,
                            ..Default::default()
                        })
                        .await?;
                    verify(!albums.iter().any(|a| &a.album_id == id), "deleted album still listed")
                })
                .await;
            report.group("cleanup/albums", result);
        }
        // Detached trashed children must be removed separately from their parent.
        for file in &self.trashed {
            match client.get_file(&file.drive_id, &file.file_id).await {
                Err(error) if error.is_not_found() => continue,
                Ok(meta) if !meta.trashed => continue,
                Err(error) => {
                    report.group("cleanup/trashed-test-files", Err(error));
                    continue;
                }
                Ok(_) => {}
            }
            let result = report
                .case("delete_permanently", async {
                    batch_ok(client.delete_permanently(&file.drive_id, &[&file.file_id]).await?, 1)?;
                    expect_missing(client.get_file(&file.drive_id, &file.file_id).await)
                })
                .await;
            report.group("cleanup/trashed-test-files", result);
        }
        for root in self.roots.iter().rev() {
            let result = report
                .case("purge", async {
                    let meta = client.get_file(&root.drive, &root.id).await?;
                    verify(owns_root(root, &meta), "refusing cleanup outside owned sandbox")?;
                    if meta.trashed {
                        batch_ok(client.delete_permanently(&root.drive, &[&root.id]).await?, 1)?;
                    } else {
                        batch_ok(client.purge(&root.drive, &[&root.id]).await?, 1)?;
                    }
                    expect_missing(client.get_file(&root.drive, &root.id).await)
                })
                .await;
            report.group("cleanup/sandboxes", result);
        }
    }
}

fn expect_missing<T>(result: Result<T>) -> Result<()> {
    match result {
        Err(error) if error.is_not_found() => Ok(()),
        Err(error) => Err(error),
        Ok(_) => Err(Error::InvalidInput("deleted resource is still accessible".into())),
    }
}

async fn account(client: &Client, report: &mut Report) {
    // These independent cases continue even if an optional endpoint is rejected.
    let _ = report
        .case("force_refresh", async {
            let before = client.credentials().await;
            client.force_refresh().await?;
            let after = client.credentials().await;
            verify(
                !after.refresh_token.is_empty()
                    && before.device_id == after.device_id
                    && !after.access_token.is_empty(),
                "refresh did not preserve usable credentials",
            )
        })
        .await;
    let _ = report.case("recreate_session", client.recreate_session()).await;
    let _ = report.case("renew_session", client.renew_session()).await;
    let _ = report
        .case("get_user_info", async {
            let user = client.get_user_info().await?;
            verify(
                !user.user_id.is_empty()
                    && user.user_id == client.user_id().await
                    && user.default_drive_id == client.default_drive_id().await,
                "inconsistent account identity",
            )
        })
        .await;
    let _ = report
        .case("get_personal_info", async {
            verify(
                client.get_personal_info().await?.personal_space_info.total_size > 0,
                "missing space quota",
            )
        })
        .await;
    let _ = report.case("get_sbox_info", client.get_sbox_info()).await;
    let _ = report
        .case("get_albums_info", async {
            verify(
                !client.get_albums_info().await?.data.drive_id.is_empty(),
                "missing album drive ID",
            )
        })
        .await;
    let _ = report
        .case("get_vip_info", async {
            verify(
                client.get_vip_info().await?.identity.is_some_and(|id| !id.is_empty()),
                "missing VIP identity",
            )
        })
        .await;
    let _ = report
        .case("list_share_links", client.list_share_links(&page_options()))
        .await;
    let _ = report
        .case("list_all_share_links", client.list_all_share_links(&page_options()))
        .await;
    let opts = ListOptions {
        order_by: OrderBy::CreatedAt,
        ..page_options()
    };
    let _ = report.case("list_albums", client.list_albums(&opts)).await;
    let _ = report.case("list_all_albums", client.list_all_albums(&opts)).await;
}

async fn files(
    client: &Client,
    report: &mut Report,
    root: &Sandbox,
    local: &Path,
    resources: &mut Resources,
) -> Result<()> {
    let drive = &root.drive;
    let parent = &root.id;
    let mut data = vec![0; 700 * 1024];
    getrandom::fill(&mut data).map_err(|_| Error::InvalidInput("random fixture generation failed".into()))?;
    let uploaded = report
        .case("upload", async {
            let up = client
                .upload(drive, parent, "payload.bin", UploadSource::Memory(&data), &options())
                .await?;
            verify(
                !up.rapid && up.file.size == data.len() as u64 && !up.file.file_id.is_empty(),
                "multipart upload mismatch",
            )?;
            Ok(up.file)
        })
        .await?;
    let id = &uploaded.file_id;
    report
        .case("get_file", async {
            let meta = client.get_file(drive, id).await?;
            verify(
                meta.file_id == *id && meta.size == data.len() as u64 && meta.sha1_matches(&sha1(&data)),
                "metadata/hash mismatch",
            )
        })
        .await?;
    report
        .case("get_path", async {
            let path = client.get_path(drive, id).await?;
            verify(
                path.first().is_some_and(|f| f.file_id == *id) && path.iter().any(|f| f.file_id == *parent),
                "path ancestry mismatch",
            )
        })
        .await?;
    report
        .case("find_child", async {
            verify(
                client
                    .find_child(drive, parent, "payload.bin")
                    .await?
                    .is_some_and(|f| f.file_id == *id),
                "child lookup mismatch",
            )
        })
        .await?;
    report
        .case("get_by_path", async {
            verify(
                client
                    .get_by_path(drive, &format!("/{}/payload.bin", root.name))
                    .await?
                    .is_some_and(|f| f.file_id == *id),
                "absolute path mismatch",
            )
        })
        .await?;
    let deep = report
        .case("mkdir_p", async {
            let path = format!("/{}/nested/deep", root.name);
            let first = client.mkdir_p(drive, &path).await?;
            let second = client.mkdir_p(drive, &path).await?;
            verify(
                first.file_id == second.file_id && first.is_folder(),
                "mkdir_p is not idempotent",
            )?;
            Ok(first)
        })
        .await?;
    report
        .case("list_files", async {
            let first = client.list_files(drive, parent, &page_options()).await?;
            let marker = first
                .next_marker()
                .ok_or_else(|| Error::InvalidInput("pagination marker missing".into()))?;
            let next = client
                .list_files(drive, parent, &page_options().with_marker(marker))
                .await?;
            verify(
                first.items.len() == 1 && next.items.len() == 1 && first.items[0].file_id != next.items[0].file_id,
                "pagination repeated or omitted entries",
            )
        })
        .await?;
    report
        .case("list_all_files", async {
            let all = client.list_all_files(drive, parent).await?;
            verify(
                all.iter().any(|f| f.file_id == *id) && all.iter().any(|f| f.name == "nested"),
                "list-all omitted test resources",
            )
        })
        .await?;
    report
        .case("walk", async {
            let entries = client.walk(drive, parent).collect().await?;
            verify(
                entries.iter().any(|e| e.path == "nested/deep") && entries.iter().any(|e| e.item.file_id == *id),
                "walk omitted nested entries",
            )
        })
        .await?;
    let url = report
        .case("get_download_url", async {
            let url = client.get_download_url(drive, id, 900).await?;
            verify(!url.url.is_empty() && !url.is_blocked(), "unusable download URL")?;
            Ok(url)
        })
        .await?;
    report
        .case("open_download_url", async {
            let mut stream = client.open_download_url(&url.url, None).await?;
            verify(stream.status() == 200, "full download status mismatch")?;
            let mut bytes = Vec::new();
            while let Some(chunk) = stream.chunk().await? {
                bytes.extend_from_slice(&chunk);
            }
            verify(bytes == data, "streamed download content mismatch")
        })
        .await?;
    report
        .case("download", async {
            let stream = client.download(drive, id, Some(100..612)).await?;
            verify(stream.status() == 206, "ranged download status mismatch")?;
            verify(
                stream.bytes().await?.as_ref() == &data[100..612],
                "range content mismatch",
            )
        })
        .await?;
    let disk = local.join("download.bin");
    report
        .case("download_to_file", async {
            client.download_to_file(drive, id, &disk).await?;
            verify(fs::read(&disk)? == data, "downloaded file content mismatch")
        })
        .await?;
    report
        .case("upload_file", async {
            let up = client.upload_file(drive, parent, &disk, &options()).await?;
            verify(
                up.rapid && up.file.size == data.len() as u64,
                "identical file did not rapid-upload",
            )
        })
        .await?;
    report
        .case("batch", async {
            let requests = [BatchRequest::new(
                "owned-file",
                "POST",
                "/file/get",
                json!({"drive_id": drive, "file_id": id}),
            )];
            let responses = batch_ok(client.batch(BatchVersion::AdriveV4, &requests, None).await?, 1)?;
            verify(
                responses[0].id == "owned-file" && responses[0].body["file_id"] == *id,
                "batch correlation mismatch",
            )
        })
        .await?;
    let moved = report
        .case("move_files", async {
            let responses = batch_ok(client.move_files(drive, &[id], drive, &deep.file_id).await?, 1)?;
            Ok(responses)
        })
        .await?;
    tasks(client, report, &moved).await?;
    report
        .case("move_files", async {
            verify(
                client.get_file(drive, id).await?.parent_file_id == deep.file_id,
                "move destination mismatch",
            )
        })
        .await?;
    report
        .case("rename", async {
            let file = client.rename(drive, id, "renamed.bin", CheckNameMode::Refuse).await?;
            verify(file.file_id == *id && file.name == "renamed.bin", "rename mismatch")
        })
        .await?;
    report
        .case("set_starred", async {
            batch_ok(client.set_starred(drive, &[id], true).await?, 1)?;
            verify(client.get_file(drive, id).await?.starred, "star state mismatch")
        })
        .await?;
    report
        .case("unstar_files", async {
            batch_ok(client.unstar_files(drive, &[id]).await?, 1)?;
            verify(!client.get_file(drive, id).await?.starred, "unstar state mismatch")
        })
        .await?;
    report
        .case("star_files", async {
            batch_ok(client.star_files(drive, &[id]).await?, 1)?;
            verify(client.get_file(drive, id).await?.starred, "star alias state mismatch")
        })
        .await?;
    // Register before trash: the server may apply a mutation even if its response is lost.
    resources.trashed.push(DriveFile::new(drive, id));
    report
        .case("trash", async {
            batch_ok(client.trash(drive, &[id]).await?, 1)?;
            Ok(())
        })
        .await?;
    report
        .case("list_recycle_bin", async {
            let page = client.list_recycle_bin(drive, &ListOptions::default()).await?;
            verify(
                page.items.iter().any(|f| f.file_id == *id),
                "newly trashed file not listed",
            )
        })
        .await?;
    report
        .case("list_all_recycle_bin", async {
            let items = client.list_all_recycle_bin(drive, &ListOptions::default()).await?;
            verify(
                items.iter().any(|f| f.file_id == *id),
                "list-all recycle bin omitted test file",
            )
        })
        .await?;
    report
        .case("restore", async {
            batch_ok(client.restore(drive, &[id]).await?, 1)?;
            verify(!client.get_file(drive, id).await?.trashed, "restore state mismatch")
        })
        .await?;
    resources.trashed.retain(|f| f.file_id != *id || f.drive_id != *drive);
    resources.trashed.push(DriveFile::new(drive, id));
    batch_ok(client.trash(drive, &[id]).await?, 1)?;
    report
        .case("delete_permanently", async {
            batch_ok(client.delete_permanently(drive, &[id]).await?, 1)?;
            expect_missing(client.get_file(drive, id).await)
        })
        .await?;
    resources.trashed.retain(|f| f.file_id != *id || f.drive_id != *drive);
    Ok(())
}

async fn multipart(client: &Client, report: &mut Report, root: &Sandbox) -> Result<()> {
    let mut data = vec![0; 700 * 1024];
    getrandom::fill(&mut data).map_err(|_| Error::InvalidInput("random fixture generation failed".into()))?;
    let drive = &root.drive;
    let start = report
        .case("begin_upload", async {
            match client
                .begin_upload(drive, &root.id, "resume.bin", UploadSource::Memory(&data), &options())
                .await?
            {
                UploadStart::Pending(state) if state.part_count == 2 => Ok(state),
                _ => Err(Error::InvalidInput("expected a two-part pending upload".into())),
            }
        })
        .await?;
    let urls = report
        .case("get_upload_url", async {
            let urls = client
                .get_upload_url(drive, &start.file_id, &start.upload_id, &[1])
                .await?;
            verify(
                urls.part_info_list.len() == 1
                    && urls.part_info_list[0].part_number == 1
                    && !urls.part_info_list[0].upload_url.is_empty(),
                "upload URL response mismatch",
            )?;
            Ok(urls)
        })
        .await?;
    report
        .case(
            "upload_part",
            client.upload_part(
                &urls.part_info_list[0].upload_url,
                Bytes::copy_from_slice(&data[..512 * 1024]),
            ),
        )
        .await?;
    report
        .case("list_uploaded_parts", async {
            let parts = client
                .list_uploaded_parts(drive, &start.file_id, &start.upload_id, None)
                .await?;
            verify(
                parts.uploaded_parts.len() == 1 && parts.uploaded_parts[0].part_number == 1,
                "uploaded parts mismatch",
            )
        })
        .await?;
    report
        .case("list_all_uploaded_parts", async {
            let parts = client
                .list_all_uploaded_parts(drive, &start.file_id, &start.upload_id)
                .await?;
            verify(
                parts.len() == 1 && parts[0].part_number == 1,
                "all uploaded parts mismatch",
            )
        })
        .await?;
    report
        .case("resume_upload", async {
            let mut state = start;
            state.completed_parts.insert(1);
            let json =
                serde_json::to_string(&state).map_err(|_| Error::InvalidInput("serialize upload checkpoint".into()))?;
            let mut state: UploadState =
                serde_json::from_str(&json).map_err(|_| Error::InvalidInput("restore upload checkpoint".into()))?;
            let mut progress = Vec::new();
            let file = client
                .resume_upload(&mut state, UploadSource::Memory(&data), |s| {
                    progress.push(s.completed_parts.len())
                })
                .await?;
            verify(
                progress == [2] && file.size == data.len() as u64 && file.sha1_matches(&sha1(&data)),
                "resume content/progress mismatch",
            )
        })
        .await?;
    let empty = report
        .case("create_upload", async {
            let empty_hash = sha1(b"");
            let response = client
                .create_upload(&CreateUpload {
                    drive_id: drive,
                    parent_file_id: &root.id,
                    name: "empty.bin",
                    size: 0,
                    content_hash: &empty_hash,
                    proof_code: "",
                    check_name_mode: CheckNameMode::Refuse,
                    part_count: 1,
                })
                .await?;
            verify(
                !response.exist && !response.file_id.is_empty(),
                "empty upload response invalid",
            )?;
            Ok(response)
        })
        .await?;
    report.set(
        "create_upload",
        Status::Passed,
        &format!(
            "empty file: rapid_upload={}, upload_id_present={}",
            empty.rapid_upload,
            empty.upload_id.as_deref().is_some_and(|id| !id.is_empty())
        ),
    );
    if empty.rapid_upload {
        let file = client.get_file(drive, &empty.file_id).await?;
        verify(
            file.size == 0 && file.sha1_matches(&sha1(b"")),
            "empty rapid upload metadata mismatch",
        )?;
        // A rapid response may retain an upload_id, but has no pending upload session.
        report.set(
            "complete_upload",
            Status::Passed,
            "completed by resume_upload; empty file completed by rapid upload",
        );
    } else if let Some(upload_id) = empty.upload_id.as_deref().filter(|id| !id.is_empty()) {
        report
            .case("complete_upload", async {
                let urls = client.get_upload_url(drive, &empty.file_id, upload_id, &[1]).await?;
                verify(
                    urls.part_info_list.len() == 1 && !urls.part_info_list[0].upload_url.is_empty(),
                    "empty upload URL missing",
                )?;
                client
                    .upload_part(&urls.part_info_list[0].upload_url, Bytes::new())
                    .await?;
                let file = client.complete_upload(drive, &empty.file_id, upload_id).await?;
                verify(
                    file.size == 0 && file.file_id == empty.file_id,
                    "empty upload completion mismatch",
                )
            })
            .await?;
    } else {
        return Err(Error::InvalidInput("empty upload lacks upload_id".into()));
    }
    Ok(())
}

async fn tasks(client: &Client, report: &mut Report, responses: &[BatchResponse]) -> Result<()> {
    for id in responses.iter().filter_map(BatchResponse::async_task_id) {
        report
            .case("get_async_task", async {
                let task = client.get_async_task(id).await?;
                verify(
                    task.async_task_id == id && (task.is_running() || task.is_finished()),
                    "task identity/state mismatch",
                )
            })
            .await?;
        report
            .case("get_async_tasks", async {
                let result = batch_ok(client.get_async_tasks(&[id], None).await?, 1)?;
                verify(result[0].body["async_task_id"] == id, "batch task identity mismatch")
            })
            .await?;
        report
            .case("wait_async_task", async {
                let task = client
                    .wait_async_task(id, Duration::from_secs(2), Duration::from_secs(60))
                    .await?;
                verify(task.is_succeed(), "async mutation did not succeed")
            })
            .await?;
    }
    Ok(())
}

async fn cross_drive(client: &Client, report: &mut Report, resources: &mut Resources, root: &Sandbox) -> Result<()> {
    let user = client.get_user_info().await?;
    if user.resource_drive_id.is_empty() || user.resource_drive_id == root.drive {
        for method in ["cross_drive_copy", "cross_drive_move"] {
            report.set(method, Status::Blocked, "account has no distinct resource drive");
        }
        return Ok(());
    }
    let resource = resources.root(client, &user.resource_drive_id, &root.name).await?;
    let source = client
        .upload(
            &root.drive,
            &root.id,
            "cross-source.txt",
            UploadSource::Memory(b"generated SDK test data"),
            &options(),
        )
        .await?
        .file;
    let copied = report
        .case("cross_drive_copy", async {
            let items = client
                .cross_drive_copy(&root.drive, &[&source.file_id], &resource.drive, &resource.id)
                .await?;
            verify(
                items.len() == 1 && items[0].is_success() && !items[0].file_id.is_empty(),
                "cross-drive copy response mismatch",
            )?;
            let file = client.get_file(&resource.drive, &items[0].file_id).await?;
            verify(
                file.parent_file_id == resource.id && file.sha1_matches(&sha1(b"generated SDK test data")),
                "cross-drive copied content mismatch",
            )?;
            Ok(file)
        })
        .await?;
    let dest = client
        .create_folder(&root.drive, &root.id, "cross-return", CheckNameMode::Refuse)
        .await?;
    verify(
        !dest.file_id.is_empty() && dest.file_id != "root",
        "invalid cross-drive destination",
    )?;
    report
        .case("cross_drive_move", async {
            let items = client
                .cross_drive_move(&resource.drive, &[&copied.file_id], &root.drive, &dest.file_id)
                .await?;
            verify(
                items.len() == 1 && items[0].is_success() && !items[0].file_id.is_empty(),
                "cross-drive move response mismatch",
            )?;
            let file = client.get_file(&root.drive, &items[0].file_id).await?;
            verify(
                file.parent_file_id == dest.file_id && file.sha1_matches(&sha1(b"generated SDK test data")),
                "cross-drive moved content mismatch",
            )?;
            expect_missing(client.get_file(&resource.drive, &copied.file_id).await)
        })
        .await
}

async fn shares(client: &Client, report: &mut Report, resources: &mut Resources, root: &Sandbox) -> Result<()> {
    let file = client
        .upload(
            &root.drive,
            &root.id,
            "share-fixture.txt",
            UploadSource::Memory(b"Public synthetic SDK test fixture. No user content."),
            &options(),
        )
        .await?
        .file;
    let files = [DriveFile::new(&root.drive, &file.file_id)];
    let _ = report
        .case("create_fast_share", async {
            let share = client.create_fast_share(&files).await?;
            verify(
                !share.share_id.is_empty() && share.share_url.is_some_and(|u| !u.is_empty()),
                "quick transfer response mismatch",
            )
        })
        .await;
    let _ = report
        .case("create_fast_share_from_ids", async {
            let share = client.create_fast_share_from_ids(&root.drive, &[&file.file_id]).await?;
            verify(
                !share.share_id.is_empty() && share.share_url.is_some_and(|u| !u.is_empty()),
                "quick transfer alias response mismatch",
            )
        })
        .await;
    let password = &root.name[root.name.len() - 4..];
    let expiration = share_expiration()?;
    let share = report
        .case("create_share_link", async {
            let share = client
                .create_share_link(&root.drive, &[&file.file_id], password, Some(&expiration))
                .await?;
            verify(!share.share_id.is_empty(), "missing share ID")?;
            resources.shares.push(share.share_id.clone());
            Ok(share)
        })
        .await?;
    let _ = report
        .case("list_all_share_links", async {
            let shares = client.list_all_share_links(&page_options()).await?;
            verify(
                shares.iter().any(|s| s.share_id == share.share_id),
                "created share missing from list-all",
            )
        })
        .await;
    let _ = report
        .case("get_share_by_anonymous", async {
            let anonymous = client.get_share_by_anonymous(&share.share_id).await?;
            verify(
                anonymous.file_count > 0 && anonymous.has_pwd,
                "anonymous share metadata mismatch",
            )
        })
        .await;
    let token = report
        .case("get_share_token", async {
            let token = client.get_share_token(&share.share_id, password).await?;
            verify(
                !token.share_token.is_empty() && token.expires_in > 0,
                "invalid share token response",
            )?;
            Ok(token)
        })
        .await?;
    let _ = report
        .case("get_share_token_by_url", async {
            let url = format!("https://www.alipan.com/s/{}", share.share_id);
            verify(
                !client
                    .get_share_token_by_url(&url, password)
                    .await?
                    .share_token
                    .is_empty(),
                "missing share token from URL",
            )
        })
        .await;
    report
        .case("list_shared_files", async {
            let page = client
                .list_shared_files(&share.share_id, &token.share_token, "root", &page_options())
                .await?;
            verify(
                page.items.len() == 1 && page.items[0].file_id == file.file_id,
                "shared file listing mismatch",
            )
        })
        .await?;
    report
        .case("list_all_shared_files", async {
            let files = client
                .list_all_shared_files(&share.share_id, &token.share_token, "root", &page_options())
                .await?;
            verify(
                files.len() == 1 && files[0].file_id == file.file_id,
                "all shared files mismatch",
            )
        })
        .await?;
    let dest = client
        .create_folder(&root.drive, &root.id, "saved-share", CheckNameMode::Refuse)
        .await?;
    verify(
        !dest.file_id.is_empty() && dest.file_id != "root",
        "invalid share-save destination",
    )?;
    let responses = report
        .case("save_from_share", async {
            batch_ok(
                client
                    .save_from_share(
                        &share.share_id,
                        &token.share_token,
                        &[&file.file_id],
                        &root.drive,
                        &dest.file_id,
                        false,
                    )
                    .await?,
                1,
            )
        })
        .await?;
    tasks(client, report, &responses).await?;
    report
        .case("save_from_share", async {
            let saved = client.list_all_files(&root.drive, &dest.file_id).await?;
            verify(
                saved.len() == 1
                    && saved[0].sha1_matches(&sha1(b"Public synthetic SDK test fixture. No user content.")),
                "saved-share content mismatch",
            )
        })
        .await
}

fn share_expiration() -> Result<String> {
    // Use the platform's date formatter instead of adding a date dependency to the SDK.
    let args: &[&str] = if cfg!(target_os = "macos") {
        &["-u", "-v+1d", "+%Y-%m-%dT%H:%M:%SZ"]
    } else {
        &["-u", "-d", "+1 day", "+%Y-%m-%dT%H:%M:%SZ"]
    };
    let output = std::process::Command::new("date").args(args).output()?;
    verify(output.status.success(), "share expiration date calculation failed")?;
    let date = String::from_utf8(output.stdout).map_err(|_| Error::InvalidInput("invalid date output".into()))?;
    Ok(date.trim().to_owned())
}

async fn albums(
    client: &Client,
    report: &mut Report,
    resources: &mut Resources,
    root: &Sandbox,
    local: &Path,
) -> Result<()> {
    let album = report
        .case("create_album", async {
            let album = client.create_album(&root.name, "Generated SDK fixtures only").await?;
            verify(!album.album_id.is_empty(), "missing album ID")?;
            resources.albums.push(album.album_id.clone());
            verify(album.name == root.name, "album name mismatch")?;
            Ok(album)
        })
        .await?;
    let id = &album.album_id;
    let _ = report
        .case("list_all_albums", async {
            let albums = client
                .list_all_albums(&ListOptions {
                    order_by: OrderBy::CreatedAt,
                    ..page_options()
                })
                .await?;
            verify(
                albums.iter().any(|a| a.album_id == *id),
                "created album missing from list-all",
            )
        })
        .await;
    let _ = report
        .case("update_album", async {
            let updated = client
                .update_album(id, &format!("{}-renamed", root.name), "Updated SDK fixture")
                .await?;
            verify(
                updated.album_id == *id && updated.name == format!("{}-renamed", root.name),
                "album update mismatch",
            )
        })
        .await;
    let _ = report
        .case("get_album", async {
            verify(client.get_album(id).await?.album_id == *id, "album identity mismatch")
        })
        .await;
    let album_drive = client.get_albums_info().await?.data.drive_id;
    verify(!album_drive.is_empty(), "album drive unavailable")?;
    let album_root = resources
        .root(client, &album_drive, &format!("{}-album", root.name))
        .await?;
    // Upload in the album drive so membership creation need not copy user-drive files.
    let photo = client
        .upload_file(
            &album_root.drive,
            &album_root.id,
            &local.join("fixture.jpg"),
            &options(),
        )
        .await?
        .file;
    let files = report
        .case("add_album_files", async {
            let added = client
                .add_album_files(id, &[DriveFile::new(&album_root.drive, &photo.file_id)])
                .await?;
            verify(
                added.len() == 1 && !added[0].drive_id.is_empty() && !added[0].file_id.is_empty(),
                "album add response mismatch",
            )?;
            let files: Vec<_> = added.iter().map(|f| DriveFile::new(&f.drive_id, &f.file_id)).collect();
            resources.album_files.push((id.clone(), files.clone()));
            verify(
                files[0].drive_id == album_root.drive && files[0].file_id == photo.file_id,
                "album membership unexpectedly copied the fixture outside its sandbox",
            )?;
            Ok(files)
        })
        .await?;
    let _ = report
        .case("list_album_files", async {
            let page = client.list_album_files(id, &page_options()).await?;
            verify(
                page.items.iter().any(|f| f.file_id == files[0].file_id),
                "album membership missing",
            )
        })
        .await;
    let _ = report
        .case("list_all_album_files", async {
            let all = client.list_all_album_files(id, &page_options()).await?;
            verify(
                all.len() == 1 && all[0].file_id == files[0].file_id,
                "all album membership mismatch",
            )
        })
        .await;
    let expiration = share_expiration()?;
    let _ = report
        .case("create_album_share", async {
            let share = client
                .create_album_share(id, &root.name[root.name.len() - 4..], Some(&expiration))
                .await?;
            verify(!share.share_id.is_empty(), "missing album share ID")?;
            resources.shares.push(share.share_id);
            Ok(())
        })
        .await;
    report
        .case("delete_album_files", async {
            client.delete_album_files(id, &files).await?;
            verify(
                client.list_all_album_files(id, &page_options()).await?.is_empty(),
                "album membership removal failed",
            )
        })
        .await?;
    resources.album_files.retain(|(album, _)| album != id);
    Ok(())
}

async fn video(client: &Client, report: &mut Report, root: &Sandbox, local: &Path) -> Result<()> {
    let uploaded = client
        .upload_file(&root.drive, &root.id, &local.join("fixture.mp4"), &options())
        .await?
        .file;
    report
        .case("get_video_preview_play_info", async {
            // Transcoding is asynchronous; a queued/running task is not a playable preview yet.
            for _ in 0..12 {
                let info = client
                    .get_video_preview_play_info(&root.drive, &uploaded.file_id)
                    .await?;
                if info
                    .video_preview_play_info
                    .live_transcoding_task_list
                    .iter()
                    .any(|task| {
                        task.status.as_deref() == Some("finished")
                            && task.url.as_deref().is_some_and(|url| !url.is_empty())
                    })
                {
                    verify(
                        info.file_id.as_deref() == Some(&uploaded.file_id),
                        "video preview identity mismatch",
                    )?;
                    return Ok(());
                }
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
            Err(Error::InvalidInput(
                "video preview did not return a finished playable task within 60 seconds".into(),
            ))
        })
        .await
}

pub async fn run<S: TokenStore + 'static>(config: Config, store: S, local: &Path) -> std::result::Result<(), String> {
    let mut report = Report::new();
    report.write(local)?;
    let connected = report.case("connect", Client::connect(config, store)).await;
    let client = match connected {
        Ok(client) => client,
        Err(_) => {
            report.write(local)?;
            return Err("API suite could not connect; see API report".into());
        }
    };
    report.set("connect_shared", Status::Passed, "exercised by Client::connect");
    let mut resources = Resources::default();
    let run_id = checked("generate sandbox ID", Credentials::from_refresh_token("fixture"))?.device_id;
    let name = format!("aliyunpan-sdk-check-{run_id}");
    println!("Test sandbox name: {name}");
    let body = async {
        account(&client, &mut report).await;
        let drive = client.default_drive_id().await;
        let root = report
            .case("create_folder", resources.root(&client, &drive, &name))
            .await?;
        let result = files(&client, &mut report, &root, local, &mut resources).await;
        report.group("group/files", result);
        let result = multipart(&client, &mut report, &root).await;
        report.group("group/multipart", result);
        let result = cross_drive(&client, &mut report, &mut resources, &root).await;
        report.group("group/cross-drive", result);
        let result = shares(&client, &mut report, &mut resources, &root).await;
        report.group("group/shares", result);
        let result = albums(&client, &mut report, &mut resources, &root, local).await;
        report.group("group/albums", result);
        let result = video(&client, &mut report, &root, local).await;
        report.group("group/video", result);
        Ok(())
    };
    match tokio::time::timeout(Duration::from_secs(15 * 60), body).await {
        Ok(result) => report.group("suite", result),
        Err(_) => report.set(
            "suite",
            Status::Failed,
            "15-minute budget exceeded; unexecuted cases remain blocked",
        ),
    }
    let checkpoint_report = report.write(local);
    if tokio::time::timeout(Duration::from_secs(5 * 60), resources.cleanup(&client, &mut report))
        .await
        .is_err()
    {
        report.set(
            "cleanup",
            Status::Failed,
            "5-minute cleanup budget exceeded; inspect the named test sandbox",
        );
    }
    report.write(local)?;
    checkpoint_report?;
    if report.failed() {
        Err("API contract tests or cleanup failed; see API report (blocked cases are not verified)".into())
    } else {
        println!("API suite passed executed cases; inspect report for blocked/excluded coverage.");
        Ok(())
    }
}

#[cfg(all(test, unix))]
#[path = "suite_tests.rs"]
mod tests;
