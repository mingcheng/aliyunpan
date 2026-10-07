use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

/// Deserialize `null` as the default value.
pub(crate) fn nullable<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FileKind {
    #[default]
    File,
    Folder,
    #[serde(other)]
    Unknown,
}

/// File or folder entry with original server fields and RFC3339 timestamp strings.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FileItem {
    #[serde(deserialize_with = "nullable")]
    pub drive_id: String,
    #[serde(deserialize_with = "nullable")]
    pub domain_id: String,
    #[serde(deserialize_with = "nullable")]
    pub file_id: String,
    #[serde(deserialize_with = "nullable")]
    pub parent_file_id: String,
    #[serde(deserialize_with = "nullable")]
    pub name: String,
    #[serde(rename = "type", deserialize_with = "nullable")]
    pub kind: FileKind,
    #[serde(deserialize_with = "nullable")]
    pub size: u64,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
    pub content_hash: Option<String>,
    pub content_hash_name: Option<String>,
    pub crc64_hash: Option<String>,
    pub content_type: Option<String>,
    pub mime_type: Option<String>,
    pub mime_extension: Option<String>,
    pub file_extension: Option<String>,
    pub category: Option<String>,
    pub status: Option<String>,
    #[serde(deserialize_with = "nullable")]
    pub starred: bool,
    #[serde(deserialize_with = "nullable")]
    pub hidden: bool,
    #[serde(deserialize_with = "nullable")]
    pub trashed: bool,
    pub encrypt_mode: Option<String>,
    pub upload_id: Option<String>,
    pub download_url: Option<String>,
    pub url: Option<String>,
    pub thumbnail: Option<String>,
    #[serde(deserialize_with = "nullable")]
    pub punish_flag: i64,
    pub local_created_at: Option<String>,
    pub local_modified_at: Option<String>,
}

impl FileItem {
    pub fn is_folder(&self) -> bool {
        self.kind == FileKind::Folder
    }

    pub fn is_file(&self) -> bool {
        self.kind == FileKind::File
    }

    /// Root placeholder; the web API does not provide a root metadata endpoint.
    pub fn root(drive_id: &str) -> Self {
        Self {
            drive_id: drive_id.into(),
            file_id: "root".into(),
            name: "/".into(),
            kind: FileKind::Folder,
            ..Self::default()
        }
    }

    /// Compare SHA1 hashes without regard to case.
    pub fn sha1_matches(&self, sha1_hex: &str) -> bool {
        self.content_hash
            .as_deref()
            .is_some_and(|h| h.eq_ignore_ascii_case(sha1_hex))
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(bound(deserialize = "T: Deserialize<'de>"))]
pub struct Page<T> {
    #[serde(default, deserialize_with = "nullable")]
    pub items: Vec<T>,
    #[serde(default, deserialize_with = "nullable")]
    pub next_marker: String,
}

impl<T> Page<T> {
    pub fn next_marker(&self) -> Option<&str> {
        Some(self.next_marker.as_str()).filter(|m| !m.is_empty())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckNameMode {
    #[default]
    Refuse,
    AutoRename,
    Overwrite,
}

/// Sort field. `FileCount` is only for album lists; `JoinedAt` is only for files within albums.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OrderBy {
    Name,
    CreatedAt,
    #[default]
    UpdatedAt,
    Size,
    FileCount,
    JoinedAt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum OrderDirection {
    Asc,
    #[default]
    Desc,
}

#[derive(Debug, Clone)]
pub struct ListOptions {
    pub limit: u32,
    pub order_by: OrderBy,
    pub order_direction: OrderDirection,
    pub marker: Option<String>,
}

impl Default for ListOptions {
    fn default() -> Self {
        Self {
            limit: 100,
            order_by: OrderBy::default(),
            order_direction: OrderDirection::default(),
            marker: None,
        }
    }
}

impl ListOptions {
    pub fn with_marker(mut self, marker: impl Into<String>) -> Self {
        self.marker = Some(marker.into());
        self
    }

    pub fn order(mut self, by: OrderBy, direction: OrderDirection) -> Self {
        self.order_by = by;
        self.order_direction = direction;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DriveFile {
    pub drive_id: String,
    pub file_id: String,
}

impl DriveFile {
    pub fn new(drive_id: impl Into<String>, file_id: impl Into<String>) -> Self {
        Self {
            drive_id: drive_id.into(),
            file_id: file_id.into(),
        }
    }
}

// ---------- Users ----------

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct UserInfo {
    #[serde(deserialize_with = "nullable")]
    pub user_id: String,
    #[serde(deserialize_with = "nullable")]
    pub default_drive_id: String,
    #[serde(deserialize_with = "nullable")]
    pub resource_drive_id: String,
    #[serde(deserialize_with = "nullable")]
    pub backup_drive_id: String,
    pub domain_id: Option<String>,
    pub avatar: Option<String>,
    pub email: Option<String>,
    pub phone: Option<String>,
    pub nick_name: Option<String>,
    pub user_name: Option<String>,
    pub description: Option<String>,
    pub role: Option<String>,
    pub status: Option<String>,
    /// Timestamp in milliseconds.
    #[serde(deserialize_with = "nullable")]
    pub created_at: i64,
    #[serde(deserialize_with = "nullable")]
    pub updated_at: i64,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct PersonalInfo {
    pub personal_rights_info: Option<RightsInfo>,
    #[serde(deserialize_with = "nullable")]
    pub personal_space_info: SpaceInfo,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct RightsInfo {
    pub spu_id: Option<String>,
    pub name: Option<String>,
    #[serde(deserialize_with = "nullable")]
    pub is_expires: bool,
    #[serde(deserialize_with = "nullable")]
    pub privileges: Vec<Privilege>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Privilege {
    pub feature_id: Option<String>,
    pub feature_attr_id: Option<String>,
    #[serde(deserialize_with = "nullable")]
    pub quota: i64,
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(default)]
pub struct SpaceInfo {
    #[serde(deserialize_with = "nullable")]
    pub used_size: u64,
    #[serde(deserialize_with = "nullable")]
    pub total_size: u64,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct SboxInfo {
    #[serde(deserialize_with = "nullable")]
    pub drive_id: String,
    #[serde(deserialize_with = "nullable")]
    pub sbox_used_size: u64,
    #[serde(deserialize_with = "nullable")]
    pub sbox_total_size: u64,
    pub recommend_vip: Option<String>,
    #[serde(deserialize_with = "nullable")]
    pub pin_setup: bool,
    #[serde(deserialize_with = "nullable")]
    pub locked: bool,
    #[serde(deserialize_with = "nullable")]
    pub insurance_enabled: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct AlbumsInfo {
    #[serde(deserialize_with = "nullable")]
    pub data: AlbumDrive,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct AlbumDrive {
    #[serde(rename = "driveId", deserialize_with = "nullable")]
    pub drive_id: String,
    #[serde(rename = "driveName")]
    pub drive_name: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct VipInfo {
    /// `member` for a standard account; `vip` for a paid member.
    pub identity: Option<String>,
    pub icon: Option<String>,
    #[serde(rename = "vipList", deserialize_with = "nullable")]
    pub vip_list: Vec<VipItem>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct VipItem {
    pub name: Option<String>,
    pub code: Option<String>,
    #[serde(rename = "promotedAt", deserialize_with = "nullable")]
    pub promoted_at: i64,
    #[serde(deserialize_with = "nullable")]
    pub expire: i64,
}

// ---------- Uploads / Downloads ----------

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct UploadPartInfo {
    #[serde(deserialize_with = "nullable")]
    pub part_number: u32,
    #[serde(deserialize_with = "nullable")]
    pub upload_url: String,
    pub internal_upload_url: Option<String>,
    pub content_type: Option<String>,
}

/// Response from `createWithFolders` when creating a folder or upload task.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct CreateFileResponse {
    #[serde(deserialize_with = "nullable")]
    pub drive_id: String,
    #[serde(deserialize_with = "nullable")]
    pub file_id: String,
    #[serde(deserialize_with = "nullable")]
    pub parent_file_id: String,
    #[serde(deserialize_with = "nullable")]
    pub file_name: String,
    #[serde(rename = "type", deserialize_with = "nullable")]
    pub kind: FileKind,
    pub domain_id: Option<String>,
    pub encrypt_mode: Option<String>,
    pub upload_id: Option<String>,
    #[serde(deserialize_with = "nullable")]
    pub rapid_upload: bool,
    /// True when `check_name_mode=refuse` finds an existing name (HTTP 200 without upload_id).
    #[serde(deserialize_with = "nullable")]
    pub exist: bool,
    #[serde(deserialize_with = "nullable")]
    pub part_info_list: Vec<UploadPartInfo>,
    pub location: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct UploadUrlResponse {
    pub domain_id: Option<String>,
    pub drive_id: Option<String>,
    pub file_id: Option<String>,
    pub upload_id: Option<String>,
    pub create_at: Option<String>,
    #[serde(deserialize_with = "nullable")]
    pub part_info_list: Vec<UploadPartInfo>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct UploadedParts {
    pub drive_id: Option<String>,
    pub upload_id: Option<String>,
    #[serde(rename = "parallelUpload", deserialize_with = "nullable")]
    pub parallel_upload: bool,
    #[serde(deserialize_with = "nullable")]
    pub uploaded_parts: Vec<UploadedPart>,
    #[serde(deserialize_with = "nullable")]
    pub next_part_number_marker: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct UploadedPart {
    pub etag: Option<String>,
    #[serde(deserialize_with = "nullable")]
    pub part_number: u32,
    #[serde(deserialize_with = "nullable")]
    pub part_size: u64,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct DownloadUrl {
    pub method: Option<String>,
    #[serde(deserialize_with = "nullable")]
    pub url: String,
    pub internal_url: Option<String>,
    pub cdn_url: Option<String>,
    pub expiration: Option<String>,
    #[serde(deserialize_with = "nullable")]
    pub size: u64,
    pub ratelimit: Option<RateLimitInfo>,
    pub description: Option<String>,
}

impl DownloadUrl {
    /// Whether the download URL points to a blocked resource.
    pub fn is_blocked(&self) -> bool {
        self.url
            .starts_with("https://pds-system-file.oss-cn-beijing.aliyuncs.com/illegal")
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(default)]
pub struct RateLimitInfo {
    #[serde(deserialize_with = "nullable")]
    pub part_speed: i64,
    #[serde(deserialize_with = "nullable")]
    pub part_size: i64,
}

// ---------- Recycle Bin / Async Tasks ----------

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct AsyncTaskRef {
    pub domain_id: Option<String>,
    pub drive_id: Option<String>,
    pub task_id: Option<String>,
    pub async_task_id: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct AsyncTask {
    #[serde(deserialize_with = "nullable")]
    pub async_task_id: String,
    /// Expected to be `Running`, `Succeed`, or `Failed`; not verified against the live API.
    pub state: Option<String>,
    pub status: Option<String>,
    #[serde(deserialize_with = "nullable")]
    pub total_process: u64,
    #[serde(deserialize_with = "nullable")]
    pub consumed_process: u64,
    #[serde(deserialize_with = "nullable")]
    pub skipped_process: u64,
    #[serde(deserialize_with = "nullable")]
    pub failed_process: u64,
    #[serde(deserialize_with = "nullable")]
    pub punished_file_count: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AsyncTaskState {
    Running,
    Succeed,
    Failed,
    Other,
}

impl AsyncTask {
    pub fn state_kind(&self) -> AsyncTaskState {
        let s = self
            .state
            .as_deref()
            .or(self.status.as_deref())
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        match s.as_str() {
            "running" | "run" | "processing" => AsyncTaskState::Running,
            "succeed" | "success" | "succeeded" | "done" | "finished" | "finish" => AsyncTaskState::Succeed,
            "failed" | "fail" | "error" => AsyncTaskState::Failed,
            _ => AsyncTaskState::Other,
        }
    }

    pub fn is_running(&self) -> bool {
        self.state_kind() == AsyncTaskState::Running
    }

    pub fn is_succeed(&self) -> bool {
        self.state_kind() == AsyncTaskState::Succeed
    }

    pub fn is_failed(&self) -> bool {
        self.state_kind() == AsyncTaskState::Failed
    }

    pub fn is_finished(&self) -> bool {
        matches!(self.state_kind(), AsyncTaskState::Succeed | AsyncTaskState::Failed)
    }
}

// ---------- Shares ----------

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ShareLink {
    #[serde(deserialize_with = "nullable")]
    pub share_id: String,
    pub share_url: Option<String>,
    pub share_pwd: Option<String>,
    pub share_name: Option<String>,
    pub share_msg: Option<String>,
    pub share_policy: Option<String>,
    pub drive_id: Option<String>,
    pub file_id: Option<String>,
    #[serde(deserialize_with = "nullable")]
    pub file_id_list: Vec<String>,
    pub expiration: Option<String>,
    #[serde(deserialize_with = "nullable")]
    pub expired: bool,
    pub status: Option<String>,
    pub creator: Option<String>,
    pub description: Option<String>,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
    #[serde(deserialize_with = "nullable")]
    pub download_count: u64,
    #[serde(deserialize_with = "nullable")]
    pub preview_count: u64,
    #[serde(deserialize_with = "nullable")]
    pub save_count: u64,
    pub first_file: Option<FileItem>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct AnonymousShare {
    pub creator_id: Option<String>,
    pub creator_name: Option<String>,
    pub creator_phone: Option<String>,
    pub expiration: Option<String>,
    pub updated_at: Option<String>,
    pub vip: Option<String>,
    pub avatar: Option<String>,
    pub share_name: Option<String>,
    pub share_title: Option<String>,
    pub display_name: Option<String>,
    #[serde(deserialize_with = "nullable")]
    pub file_count: u64,
    #[serde(deserialize_with = "nullable")]
    pub has_pwd: bool,
    #[serde(deserialize_with = "nullable")]
    pub is_creator_followable: bool,
    #[serde(deserialize_with = "nullable")]
    pub is_following_creator: bool,
    pub save_button: Option<Value>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ShareToken {
    #[serde(deserialize_with = "nullable")]
    pub share_token: String,
    #[serde(deserialize_with = "nullable")]
    pub expires_in: i64,
    pub expire_time: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct SharedFile {
    #[serde(deserialize_with = "nullable")]
    pub share_id: String,
    #[serde(deserialize_with = "nullable")]
    pub drive_id: String,
    #[serde(deserialize_with = "nullable")]
    pub file_id: String,
    #[serde(deserialize_with = "nullable")]
    pub parent_file_id: String,
    #[serde(deserialize_with = "nullable")]
    pub name: String,
    #[serde(rename = "type", deserialize_with = "nullable")]
    pub kind: FileKind,
    #[serde(deserialize_with = "nullable")]
    pub size: u64,
    pub domain_id: Option<String>,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
    pub file_extension: Option<String>,
    pub mime_type: Option<String>,
    pub mime_extension: Option<String>,
    pub category: Option<String>,
    pub thumbnail: Option<String>,
    pub revision_id: Option<String>,
    #[serde(deserialize_with = "nullable")]
    pub punish_flag: i64,
    pub image_media_metadata: Option<Value>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct FastShare {
    #[serde(deserialize_with = "nullable")]
    pub share_id: String,
    pub share_url: Option<String>,
    pub share_name: Option<String>,
    pub share_title: Option<String>,
    pub share_subtitle: Option<String>,
    pub full_share_msg: Option<String>,
    pub expiration: Option<String>,
    pub thumbnail: Option<String>,
    #[serde(deserialize_with = "nullable")]
    pub expired: bool,
    #[serde(deserialize_with = "nullable")]
    pub drive_file_list: Vec<DriveFile>,
}

// ---------- Cross-Drive Operations / Video / Albums ----------

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct CrossDriveItem {
    #[serde(deserialize_with = "nullable")]
    pub drive_id: String,
    #[serde(deserialize_with = "nullable")]
    pub file_id: String,
    #[serde(deserialize_with = "nullable")]
    pub source_drive_id: String,
    #[serde(deserialize_with = "nullable")]
    pub source_file_id: String,
    #[serde(deserialize_with = "nullable")]
    pub status: u16,
}

impl CrossDriveItem {
    pub fn is_success(&self) -> bool {
        self.status == 201
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct VideoPreviewPlayInfo {
    pub domain_id: Option<String>,
    pub drive_id: Option<String>,
    pub file_id: Option<String>,
    #[serde(deserialize_with = "nullable")]
    pub video_preview_play_info: VideoPreview,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct VideoPreview {
    pub category: Option<String>,
    pub meta: Option<VideoMeta>,
    #[serde(deserialize_with = "nullable")]
    pub live_transcoding_task_list: Vec<TranscodingTask>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct VideoMeta {
    #[serde(deserialize_with = "nullable")]
    pub duration: f64,
    #[serde(deserialize_with = "nullable")]
    pub width: u32,
    #[serde(deserialize_with = "nullable")]
    pub height: u32,
    pub live_transcoding_meta: Option<Value>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct TranscodingTask {
    pub template_id: Option<String>,
    pub template_name: Option<String>,
    #[serde(deserialize_with = "nullable")]
    pub template_width: u32,
    #[serde(deserialize_with = "nullable")]
    pub template_height: u32,
    pub status: Option<String>,
    pub stage: Option<String>,
    pub url: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Album {
    #[serde(deserialize_with = "nullable")]
    pub album_id: String,
    #[serde(deserialize_with = "nullable")]
    pub name: String,
    pub description: Option<String>,
    pub owner: Option<String>,
    #[serde(deserialize_with = "nullable")]
    pub file_count: u64,
    #[serde(deserialize_with = "nullable")]
    pub image_count: u64,
    #[serde(deserialize_with = "nullable")]
    pub video_count: u64,
    /// Timestamp in milliseconds.
    #[serde(deserialize_with = "nullable")]
    pub created_at: i64,
    #[serde(deserialize_with = "nullable")]
    pub updated_at: i64,
    #[serde(deserialize_with = "nullable")]
    pub is_sharing: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_item_tolerates_nulls_and_missing_fields() {
        let item: FileItem = serde_json::from_str(
            r#"{"drive_id":"d","file_id":"f","name":"a.bin","type":"file","size":null,
                "parent_file_id":null,"starred":null,"content_hash":"ABC","unknown_field":1}"#,
        )
        .unwrap();
        assert_eq!(item.size, 0);
        assert_eq!(item.parent_file_id, "");
        assert!(item.is_file());
        assert!(item.sha1_matches("abc"));
        assert!(!item.sha1_matches("abd"));
    }

    #[test]
    fn unknown_file_kind() {
        let item: FileItem = serde_json::from_str(r#"{"type":"symlink"}"#).unwrap();
        assert_eq!(item.kind, FileKind::Unknown);
        let item: FileItem = serde_json::from_str(r#"{"type":"folder"}"#).unwrap();
        assert!(item.is_folder());
    }

    #[test]
    fn page_marker() {
        let p: Page<FileItem> = serde_json::from_str(r#"{"items":[{"file_id":"x"}],"next_marker":""}"#).unwrap();
        assert_eq!(p.items.len(), 1);
        assert_eq!(p.next_marker(), None);
        let p: Page<FileItem> = serde_json::from_str(r#"{"items":null,"next_marker":"m"}"#).unwrap();
        assert!(p.items.is_empty());
        assert_eq!(p.next_marker(), Some("m"));
    }

    #[test]
    fn enums_serialize_as_api_strings() {
        let v = serde_json::json!({
            "a": CheckNameMode::AutoRename,
            "b": OrderBy::UpdatedAt,
            "c": OrderDirection::Desc,
            "d": OrderBy::FileCount,
        });
        assert_eq!(
            v.to_string(),
            r#"{"a":"auto_rename","b":"updated_at","c":"DESC","d":"file_count"}"#
        );
    }

    #[test]
    fn albums_info_camel_case() {
        let info: AlbumsInfo =
            serde_json::from_str(r#"{"code":"","data":{"driveId":"123","driveName":"alibum"}}"#).unwrap();
        assert_eq!(info.data.drive_id, "123");
    }

    #[test]
    fn blocked_download_url() {
        let d = DownloadUrl {
            url: "https://pds-system-file.oss-cn-beijing.aliyuncs.com/illegal.mp4".into(),
            ..Default::default()
        };
        assert!(d.is_blocked());
    }

    #[test]
    fn async_task_state_helpers() {
        let running: AsyncTask = serde_json::from_str(r#"{"state":"Running"}"#).unwrap();
        assert_eq!(running.state_kind(), AsyncTaskState::Running);
        assert!(running.is_running());
        assert!(!running.is_finished());

        let success: AsyncTask = serde_json::from_str(r#"{"status":"Succeed"}"#).unwrap();
        assert!(success.is_succeed());
        assert!(success.is_finished());

        let failed: AsyncTask = serde_json::from_str(r#"{"state":"Failed"}"#).unwrap();
        assert!(failed.is_failed());
        assert!(failed.is_finished());

        let other: AsyncTask = serde_json::from_str(r#"{"state":"Queued"}"#).unwrap();
        assert_eq!(other.state_kind(), AsyncTaskState::Other);
        assert!(!other.is_finished());
    }
}
