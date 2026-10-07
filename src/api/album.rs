use serde::{Deserialize, de::IgnoredAny};
use serde_json::json;

use super::{IMAGE_THUMBNAIL_PROCESS, IMAGE_URL_PROCESS, VIDEO_THUMBNAIL_PROCESS, set_marker};
use crate::{
    client::Client,
    error::Result,
    models::{Album, DriveFile, FileItem, ListOptions, OrderBy, Page, ShareLink, nullable},
};

#[derive(Deserialize)]
struct FileList {
    #[serde(default, deserialize_with = "nullable")]
    file_list: Vec<FileItem>,
}

impl Client {
    /// Supported `order_by` values are `CreatedAt`, `UpdatedAt`, and `FileCount`.
    pub async fn list_albums(&self, opts: &ListOptions) -> Result<Page<Album>> {
        let mut body = json!({
            "limit": opts.limit,
            "order_by": opts.order_by,
            "order_direction": opts.order_direction,
        });
        set_marker(&mut body, opts.marker.as_deref());
        self.post("/adrive/v1/album/list", &body).await
    }

    /// List all albums, fetching subsequent pages automatically.
    pub async fn list_all_albums(&self, opts: &ListOptions) -> Result<Vec<Album>> {
        let mut opts = opts.clone();
        let mut out = Vec::new();
        loop {
            let page = self.list_albums(&opts).await?;
            let marker = page.next_marker().map(str::to_owned);
            out.extend(page.items);
            match marker {
                Some(m) => {
                    opts.marker = Some(m);
                    tokio::time::sleep(self.config().page_delay).await;
                }
                None => return Ok(out),
            }
        }
    }

    pub async fn create_album(&self, name: &str, description: &str) -> Result<Album> {
        self.post(
            "/adrive/v1/album/create",
            &json!({ "name": name, "description": description }),
        )
        .await
    }

    pub async fn update_album(&self, album_id: &str, name: &str, description: &str) -> Result<Album> {
        let body = json!({ "album_id": album_id, "name": name, "description": description });
        self.post("/adrive/v1/album/update", &body).await
    }

    pub async fn delete_album(&self, album_id: &str) -> Result<()> {
        let _: IgnoredAny = self
            .post("/adrive/v1/album/delete", &json!({ "album_id": album_id }))
            .await?;
        Ok(())
    }

    pub async fn get_album(&self, album_id: &str) -> Result<Album> {
        self.post("/adrive/v1/album/get", &json!({ "album_id": album_id }))
            .await
    }

    /// List album files. The default `UpdatedAt` order sorts by `joined_at`.
    pub async fn list_album_files(&self, album_id: &str, opts: &ListOptions) -> Result<Page<FileItem>> {
        let order_by = match opts.order_by {
            OrderBy::UpdatedAt => OrderBy::JoinedAt,
            other => other,
        };
        let mut body = json!({
            "album_id": album_id,
            "image_thumbnail_process": IMAGE_THUMBNAIL_PROCESS,
            "video_thumbnail_process": VIDEO_THUMBNAIL_PROCESS,
            "image_url_process": IMAGE_URL_PROCESS,
            "filter": "",
            "fields": "*",
            "limit": opts.limit,
            "order_by": order_by,
            "order_direction": opts.order_direction,
        });
        set_marker(&mut body, opts.marker.as_deref());
        self.post("/adrive/v1/album/list_files", &body).await
    }

    /// List all files in an album, fetching subsequent pages automatically.
    pub async fn list_all_album_files(&self, album_id: &str, opts: &ListOptions) -> Result<Vec<FileItem>> {
        let mut opts = opts.clone();
        let mut out = Vec::new();
        loop {
            let page = self.list_album_files(album_id, &opts).await?;
            let marker = page.next_marker().map(str::to_owned);
            out.extend(page.items);
            match marker {
                Some(m) => {
                    opts.marker = Some(m);
                    tokio::time::sleep(self.config().page_delay).await;
                }
                None => return Ok(out),
            }
        }
    }

    /// Returned files may belong to the album drive rather than the source file's drive.
    pub async fn add_album_files(&self, album_id: &str, files: &[DriveFile]) -> Result<Vec<FileItem>> {
        let body = json!({ "album_id": album_id, "drive_file_list": files });
        let resp: FileList = self.post("/adrive/v1/album/add_files", &body).await?;
        Ok(resp.file_list)
    }

    pub async fn delete_album_files(&self, album_id: &str, files: &[DriveFile]) -> Result<()> {
        let body = json!({ "album_id": album_id, "drive_file_list": files });
        let _: IgnoredAny = self.post("/adrive/v1/album/delete_files", &body).await?;
        Ok(())
    }

    /// Live testing returned a server message requiring an upgrade to use this feature.
    pub async fn create_album_share(
        &self,
        album_id: &str,
        share_pwd: &str,
        expiration: Option<&str>,
    ) -> Result<ShareLink> {
        let body = json!({
            "album_id": album_id,
            "share_pwd": share_pwd,
            "expiration": expiration.unwrap_or_default(),
        });
        self.post("/adrive/v2/share_link/create", &body).await
    }
}
