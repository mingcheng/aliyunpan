use serde::Deserialize;
use serde_json::json;

use super::{IMAGE_THUMBNAIL_PROCESS, IMAGE_URL_PROCESS, VIDEO_THUMBNAIL_PROCESS, ids, set_marker};
use crate::{
    client::Client,
    error::{Error, Result},
    models::{
        CheckNameMode, CreateFileResponse, CrossDriveItem, FileItem, ListOptions, Page, VideoPreviewPlayInfo, nullable,
    },
    util::validate_file_name,
};

#[derive(Deserialize)]
#[serde(bound(deserialize = "T: Deserialize<'de>"))]
struct Items<T> {
    #[serde(default, deserialize_with = "nullable")]
    items: Vec<T>,
}

impl Client {
    /// List one page of child entries.
    pub async fn list_files(&self, drive_id: &str, parent_file_id: &str, opts: &ListOptions) -> Result<Page<FileItem>> {
        let mut body = json!({
            "drive_id": drive_id,
            "parent_file_id": parent_file_id,
            "limit": opts.limit,
            "all": false,
            "url_expire_sec": 1600,
            "image_thumbnail_process": IMAGE_THUMBNAIL_PROCESS,
            "image_url_process": IMAGE_URL_PROCESS,
            "video_thumbnail_process": VIDEO_THUMBNAIL_PROCESS,
            "fields": "*",
            "order_by": opts.order_by,
            "order_direction": opts.order_direction,
        });
        set_marker(&mut body, opts.marker.as_deref());
        self.post("/adrive/v3/file/list", &body).await
    }

    pub async fn get_file(&self, drive_id: &str, file_id: &str) -> Result<FileItem> {
        self.post("/v2/file/get", &json!({ "drive_id": drive_id, "file_id": file_id }))
            .await
    }

    /// Return `[self, parent, grandparent, ...]`.
    pub async fn get_path(&self, drive_id: &str, file_id: &str) -> Result<Vec<FileItem>> {
        let resp: Items<FileItem> = self
            .post(
                "/adrive/v1/file/get_path",
                &json!({ "drive_id": drive_id, "file_id": file_id }),
            )
            .await?;
        Ok(resp.items)
    }

    pub async fn create_folder(
        &self,
        drive_id: &str,
        parent_file_id: &str,
        name: &str,
        mode: CheckNameMode,
    ) -> Result<CreateFileResponse> {
        validate_file_name(name)?;
        let body = json!({
            "drive_id": drive_id,
            "parent_file_id": parent_file_id,
            "name": name,
            "check_name_mode": mode,
            "type": "folder",
        });
        self.post("/adrive/v2/file/createWithFolders", &body).await
    }

    pub async fn rename(&self, drive_id: &str, file_id: &str, new_name: &str, mode: CheckNameMode) -> Result<FileItem> {
        validate_file_name(new_name)?;
        let body = json!({ "drive_id": drive_id, "file_id": file_id, "name": new_name, "check_name_mode": mode });
        self.post("/adrive/v3/file/update", &body).await
    }

    /// Trigger server-side transcoding.
    pub async fn get_video_preview_play_info(&self, drive_id: &str, file_id: &str) -> Result<VideoPreviewPlayInfo> {
        let body =
            json!({ "category": "live_transcoding", "drive_id": drive_id, "file_id": file_id, "template_id": "" });
        self.post("/v2/file/get_video_preview_play_info", &body).await
    }

    /// Copy across drives; source and destination drive IDs must differ.
    pub async fn cross_drive_copy<S: AsRef<str>>(
        &self,
        from_drive_id: &str,
        file_ids: &[S],
        to_drive_id: &str,
        to_parent_file_id: &str,
    ) -> Result<Vec<CrossDriveItem>> {
        self.cross_drive(
            "/adrive/v2/file/crossDriveCopy",
            from_drive_id,
            file_ids,
            to_drive_id,
            to_parent_file_id,
        )
        .await
    }

    /// Move across drives; only resource-to-backup transfers are supported.
    pub async fn cross_drive_move<S: AsRef<str>>(
        &self,
        from_drive_id: &str,
        file_ids: &[S],
        to_drive_id: &str,
        to_parent_file_id: &str,
    ) -> Result<Vec<CrossDriveItem>> {
        self.cross_drive(
            "/adrive/v2/file/crossDriveMove",
            from_drive_id,
            file_ids,
            to_drive_id,
            to_parent_file_id,
        )
        .await
    }

    async fn cross_drive<S: AsRef<str>>(
        &self,
        path: &str,
        from_drive_id: &str,
        file_ids: &[S],
        to_drive_id: &str,
        to_parent_file_id: &str,
    ) -> Result<Vec<CrossDriveItem>> {
        if from_drive_id == to_drive_id {
            return Err(Error::InvalidInput(
                "cross drive operation requires different drives".into(),
            ));
        }
        let body = json!({
            "from_drive_id": from_drive_id,
            "from_file_ids": ids(file_ids),
            "to_drive_id": to_drive_id,
            // The server requires these mixed-case field names.
            "to_parent_fileId": to_parent_file_id,
        });
        let resp: Items<CrossDriveItem> = self.post(path, &body).await?;
        Ok(resp.items)
    }
}
