use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

use bytes::Bytes;
use md5::Md5;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha1::{Digest, Sha1};
use tokio::io::{AsyncReadExt, AsyncSeekExt};

use crate::{
    client::Client,
    error::{ApiError, ApiErrorKind, Error, Result},
    models::{CheckNameMode, CreateFileResponse, FileItem, FileKind, UploadUrlResponse, UploadedPart, UploadedParts},
    util::{self, validate_file_name},
};

pub const MAX_PARTS: u64 = 10_000;
const MIB: u64 = 1024 * 1024;
const URL_REFRESH_WINDOW: usize = 100;

/// Choose a chunk size that keeps the part count at or below 10,000, rounding increases up to a MiB.
pub fn chunk_size_for(size: u64, preferred: u64) -> u64 {
    let preferred = preferred.max(1);
    if size.div_ceil(preferred) <= MAX_PARTS {
        return preferred;
    }
    let required = size.div_ceil(MAX_PARTS);
    required.div_ceil(MIB).checked_mul(MIB).unwrap_or(required)
}

/// Number of parts; even an empty file requires one part.
pub fn part_count(size: u64, chunk_size: u64) -> u32 {
    u32::try_from(size.div_ceil(chunk_size.max(1)).max(1)).unwrap_or(u32::MAX)
}

/// Return `(offset, len)` for the one-based `part_number`.
pub fn part_range(part_number: u32, chunk_size: u64, size: u64) -> (u64, u64) {
    let start = u64::from(part_number - 1) * chunk_size;
    let end = (start + chunk_size).min(size);
    (start, end.saturating_sub(start))
}

/// Byte range `[start, end)` for proof_code, derived from the access token's MD5 and file size.
pub fn proof_range(access_token: &str, size: u64) -> Option<(u64, u64)> {
    if size == 0 {
        return None;
    }
    let digest = Md5::digest(access_token.as_bytes());
    let h = u64::from_be_bytes(digest[..8].try_into().expect("md5 digest has 16 bytes"));
    let start = h % size;
    Some((start, (start + 8).min(size)))
}

/// Source of upload data.
#[derive(Debug, Clone, Copy)]
pub enum UploadSource<'a> {
    File(&'a Path),
    Memory(&'a [u8]),
}

impl UploadSource<'_> {
    async fn size(&self) -> Result<u64> {
        match self {
            Self::File(p) => Ok(tokio::fs::metadata(p).await?.len()),
            Self::Memory(b) => Ok(b.len() as u64),
        }
    }

    async fn sha1(&self) -> Result<String> {
        let mut hasher = Sha1::new();
        match self {
            Self::Memory(b) => hasher.update(b),
            Self::File(p) => {
                let mut file = tokio::fs::File::open(p).await?;
                let mut buf = vec![0u8; MIB as usize];
                loop {
                    let n = file.read(&mut buf).await?;
                    if n == 0 {
                        break;
                    }
                    hasher.update(&buf[..n]);
                }
            }
        }
        Ok(util::hex_upper(&hasher.finalize()))
    }

    async fn read(&self, offset: u64, len: u64) -> Result<Bytes> {
        match self {
            Self::Memory(b) => {
                let start = usize::try_from(offset).map_err(|_| Error::InvalidInput("offset overflow".into()))?;
                let end = start + len as usize;
                b.get(start..end)
                    .map(Bytes::copy_from_slice)
                    .ok_or_else(|| Error::InvalidInput("read past end of buffer".into()))
            }
            Self::File(p) => {
                let mut file = tokio::fs::File::open(p).await?;
                file.seek(std::io::SeekFrom::Start(offset)).await?;
                let mut buf = vec![0u8; len as usize];
                file.read_exact(&mut buf).await?;
                Ok(Bytes::from(buf))
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct UploadOptions {
    /// Prefer `Refuse` or `Overwrite` for backups.
    pub check_name_mode: CheckNameMode,
    /// Override `Config::chunk_size`.
    pub chunk_size: Option<u64>,
}

impl Default for UploadOptions {
    fn default() -> Self {
        Self {
            check_name_mode: CheckNameMode::Refuse,
            chunk_size: None,
        }
    }
}

/// Serializable upload progress for resuming interrupted transfers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UploadState {
    pub drive_id: String,
    pub file_id: String,
    pub upload_id: String,
    pub file_name: String,
    pub chunk_size: u64,
    pub part_count: u32,
    pub completed_parts: BTreeSet<u32>,
    pub local_size: u64,
    /// Uppercase hexadecimal SHA1.
    pub local_sha1: String,
    #[serde(skip)]
    urls: BTreeMap<u32, String>,
}

impl UploadState {
    pub fn is_complete(&self) -> bool {
        self.completed_parts.len() as u32 >= self.part_count
    }

    pub fn uploaded_bytes(&self) -> u64 {
        self.completed_parts
            .iter()
            .map(|&n| part_range(n, self.chunk_size, self.local_size).1)
            .sum()
    }
}

#[derive(Debug, Clone)]
pub enum UploadStart {
    /// Rapid upload succeeded; no data transfer is required.
    Rapid(Box<FileItem>),
    Pending(UploadState),
}

#[derive(Debug, Clone)]
pub struct UploadOutcome {
    pub file: FileItem,
    pub rapid: bool,
}

/// Parameters for creating an upload task with `createWithFolders`.
#[derive(Debug, Clone)]
pub struct CreateUpload<'a> {
    pub drive_id: &'a str,
    pub parent_file_id: &'a str,
    pub name: &'a str,
    pub size: u64,
    /// SHA1 of the entire file, encoded as uppercase hexadecimal.
    pub content_hash: &'a str,
    pub proof_code: &'a str,
    pub check_name_mode: CheckNameMode,
    pub part_count: u32,
}

impl Client {
    pub async fn create_upload(&self, req: &CreateUpload<'_>) -> Result<CreateFileResponse> {
        validate_file_name(req.name)?;
        if u64::from(req.part_count.max(1)) > MAX_PARTS {
            return Err(Error::InvalidInput(format!("part count exceeds {MAX_PARTS}")));
        }
        let parts: Vec<_> = (1..=req.part_count.max(1))
            .map(|n| json!({ "part_number": n }))
            .collect();
        let body = json!({
            "name": req.name,
            "drive_id": req.drive_id,
            "parent_file_id": req.parent_file_id,
            "size": req.size,
            "part_info_list": parts,
            "content_hash": req.content_hash,
            "content_hash_name": "sha1",
            "type": "file",
            "check_name_mode": req.check_name_mode,
            "proof_code": req.proof_code,
            "proof_version": "v1",
        });
        self.post("/adrive/v2/file/createWithFolders", &body).await
    }

    pub async fn get_upload_url(
        &self,
        drive_id: &str,
        file_id: &str,
        upload_id: &str,
        part_numbers: &[u32],
    ) -> Result<UploadUrlResponse> {
        let parts: Vec<_> = part_numbers.iter().map(|n| json!({ "part_number": n })).collect();
        let body = json!({ "drive_id": drive_id, "file_id": file_id, "upload_id": upload_id, "part_info_list": parts });
        self.post("/v2/file/get_upload_url", &body).await
    }

    pub async fn complete_upload(&self, drive_id: &str, file_id: &str, upload_id: &str) -> Result<FileItem> {
        let body = json!({ "ignoreError": true, "drive_id": drive_id, "file_id": file_id, "upload_id": upload_id });
        self.post("/v2/file/complete", &body).await
    }

    /// List received parts. Omit the initial marker; an empty string causes InvalidRequestJSONFormat.
    pub async fn list_uploaded_parts(
        &self,
        drive_id: &str,
        file_id: &str,
        upload_id: &str,
        part_number_marker: Option<u32>,
    ) -> Result<UploadedParts> {
        let mut body = json!({ "drive_id": drive_id, "file_id": file_id, "upload_id": upload_id });
        if let Some(marker) = part_number_marker {
            body["part_number_marker"] = marker.into();
        }
        self.post("/v2/file/list_uploaded_parts", &body).await
    }

    /// List all received parts, fetching subsequent pages automatically.
    pub async fn list_all_uploaded_parts(
        &self,
        drive_id: &str,
        file_id: &str,
        upload_id: &str,
    ) -> Result<Vec<UploadedPart>> {
        let mut marker = None;
        let mut out = Vec::new();
        loop {
            let page = self.list_uploaded_parts(drive_id, file_id, upload_id, marker).await?;
            let UploadedParts {
                uploaded_parts,
                next_part_number_marker,
                ..
            } = page;
            out.extend(uploaded_parts);
            let next = next_part_number_marker.trim();
            if next.is_empty() {
                return Ok(out);
            }
            marker = Some(next.parse().map_err(|_| {
                Error::InvalidInput(format!("invalid next_part_number_marker: {next_part_number_marker}"))
            })?);
            tokio::time::sleep(self.config().page_delay).await;
        }
    }

    /// PUT a part to a presigned URL without Content-Type; 409 (part already exists) counts as success.
    pub async fn upload_part(&self, upload_url: &str, data: Bytes) -> Result<()> {
        let referer = self.config().referer();
        let resp = self
            .send_transfer(|http| http.put(upload_url).header("referer", &referer).body(data.clone()))
            .await?;
        let status = resp.status().as_u16();
        if (200..300).contains(&status) || status == 409 {
            return Ok(());
        }
        let body = resp.text().await.unwrap_or_default();
        Err(Error::Http { status, body })
    }

    /// Compute the hash and proof_code, then create an upload task or return the file on a rapid-upload hit.
    pub async fn begin_upload(
        &self,
        drive_id: &str,
        parent_file_id: &str,
        name: &str,
        source: UploadSource<'_>,
        opts: &UploadOptions,
    ) -> Result<UploadStart> {
        validate_file_name(name)?;
        let size = source.size().await?;
        let sha1 = source.sha1().await?;
        let chunk_size = chunk_size_for(size, opts.chunk_size.unwrap_or(self.config().chunk_size));
        let parts = part_count(size, chunk_size);

        let mut retried = false;
        let resp = loop {
            // proof_code is bound to the request's access token and must be recomputed after rotation.
            let token = self.access_token().await?;
            let proof = match proof_range(&token, size) {
                Some((start, end)) => util::base64_encode(&source.read(start, end - start).await?),
                None => String::new(),
            };
            let req = CreateUpload {
                drive_id,
                parent_file_id,
                name,
                size,
                content_hash: &sha1,
                proof_code: &proof,
                check_name_mode: opts.check_name_mode,
                part_count: parts,
            };
            match self.create_upload(&req).await {
                Err(e) if !retried && e.api_kind() == Some(ApiErrorKind::InvalidRapidProof) => retried = true,
                other => break other?,
            }
        };

        if resp.exist {
            return Err(Error::Api(ApiError {
                code: "AlreadyExist.File".into(),
                message: format!("{name} already exists (file_id {})", resp.file_id),
                display_message: None,
                http_status: 200,
            }));
        }
        if resp.rapid_upload {
            return self
                .get_file(&resp.drive_id, &resp.file_id)
                .await
                .map(|f| UploadStart::Rapid(Box::new(f)));
        }
        let upload_id = resp
            .upload_id
            .filter(|id| !id.is_empty())
            .ok_or_else(|| Error::InvalidInput("create upload response lacks upload_id".into()))?;
        Ok(UploadStart::Pending(UploadState {
            drive_id: if resp.drive_id.is_empty() {
                drive_id.into()
            } else {
                resp.drive_id
            },
            file_id: resp.file_id,
            upload_id,
            file_name: if resp.file_name.is_empty() {
                name.into()
            } else {
                resp.file_name
            },
            chunk_size,
            part_count: parts,
            completed_parts: BTreeSet::new(),
            local_size: size,
            local_sha1: sha1,
            urls: resp
                .part_info_list
                .into_iter()
                .map(|p| (p.part_number, p.upload_url))
                .collect(),
        }))
    }

    /// Verify the source, upload remaining parts, and complete the upload.
    /// Call `on_progress` after each part so the caller can persist the updated state.
    pub async fn resume_upload(
        &self,
        state: &mut UploadState,
        source: UploadSource<'_>,
        mut on_progress: impl FnMut(&UploadState),
    ) -> Result<FileItem> {
        let size = source.size().await?;
        if size != state.local_size {
            return Err(Error::InvalidInput(format!(
                "local file size changed: expected {}, got {size}",
                state.local_size
            )));
        }
        let sha1 = source.sha1().await?;
        if !sha1.eq_ignore_ascii_case(&state.local_sha1) {
            return Err(Error::Integrity {
                expected: state.local_sha1.clone(),
                actual: sha1,
            });
        }
        let missing: Vec<u32> = (1..=state.part_count)
            .filter(|n| !state.completed_parts.contains(n))
            .collect();
        for (idx, &part) in missing.iter().enumerate() {
            let (offset, len) = part_range(part, state.chunk_size, state.local_size);
            let data = source.read(offset, len).await?;
            let mut refreshed = false;
            loop {
                if !state.urls.contains_key(&part) {
                    self.refresh_upload_urls(state, &missing[idx..]).await?;
                    refreshed = true;
                }
                let url = state
                    .urls
                    .get(&part)
                    .cloned()
                    .ok_or_else(|| Error::InvalidInput(format!("no upload url for part {part}")))?;
                match self.upload_part(&url, data.clone()).await {
                    Ok(()) => break,
                    Err(Error::Http { status: 403, .. }) if !refreshed => {
                        state.urls.clear();
                    }
                    Err(e) => return Err(e),
                }
            }
            state.urls.remove(&part);
            state.completed_parts.insert(part);
            on_progress(state);
        }

        let file = self
            .complete_upload(&state.drive_id, &state.file_id, &state.upload_id)
            .await?;
        if let Some(hash) = file
            .content_hash
            .as_deref()
            .filter(|hash| !hash.is_empty() && !hash.eq_ignore_ascii_case(&state.local_sha1))
        {
            return Err(Error::Integrity {
                expected: state.local_sha1.clone(),
                actual: hash.to_owned(),
            });
        }
        if file.kind == FileKind::File && file.size != state.local_size {
            return Err(Error::Integrity {
                expected: format!("{} bytes", state.local_size),
                actual: format!("{} bytes", file.size),
            });
        }
        Ok(file)
    }

    async fn refresh_upload_urls(&self, state: &mut UploadState, remaining: &[u32]) -> Result<()> {
        let window = &remaining[..remaining.len().min(URL_REFRESH_WINDOW)];
        let resp = self
            .get_upload_url(&state.drive_id, &state.file_id, &state.upload_id, window)
            .await?;
        state
            .urls
            .extend(resp.part_info_list.into_iter().map(|p| (p.part_number, p.upload_url)));
        Ok(())
    }

    /// Full upload flow: rapid-upload check, multipart transfer, completion, and integrity verification.
    pub async fn upload(
        &self,
        drive_id: &str,
        parent_file_id: &str,
        name: &str,
        source: UploadSource<'_>,
        opts: &UploadOptions,
    ) -> Result<UploadOutcome> {
        match self.begin_upload(drive_id, parent_file_id, name, source, opts).await? {
            UploadStart::Rapid(file) => Ok(UploadOutcome {
                file: *file,
                rapid: true,
            }),
            UploadStart::Pending(mut state) => {
                let file = self.resume_upload(&mut state, source, |_| {}).await?;
                Ok(UploadOutcome { file, rapid: false })
            }
        }
    }

    /// Upload a local file using its local name as the remote file name.
    pub async fn upload_file(
        &self,
        drive_id: &str,
        parent_file_id: &str,
        local: &Path,
        opts: &UploadOptions,
    ) -> Result<UploadOutcome> {
        let name = local
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| Error::InvalidInput(format!("invalid local file name: {}", local.display())))?;
        self.upload(drive_id, parent_file_id, name, UploadSource::File(local), opts)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunk_sizes() {
        let k512 = 512 * 1024;
        assert_eq!(chunk_size_for(0, k512), k512);
        assert_eq!(chunk_size_for(k512 * MAX_PARTS, k512), k512);
        let big = k512 * MAX_PARTS + 1;
        let chunk = chunk_size_for(big, k512);
        assert_eq!(chunk % MIB, 0);
        assert!(big.div_ceil(chunk) <= MAX_PARTS);
        let five_gib = 5 * 1024 * MIB;
        let five_gib_chunk = chunk_size_for(five_gib, k512);
        assert_eq!(five_gib_chunk, MIB);
        assert!(five_gib.div_ceil(five_gib_chunk) <= MAX_PARTS);

        let huge = 200 * 1024 * MIB;
        let huge_chunk = chunk_size_for(huge, k512);
        assert_eq!(huge_chunk, 21 * MIB);
        assert!(huge.div_ceil(huge_chunk) <= MAX_PARTS);

        let max_chunk = chunk_size_for(u64::MAX, k512);
        assert!(u64::MAX.div_ceil(max_chunk) <= MAX_PARTS);
        assert_eq!(part_count(u64::MAX, 1), u32::MAX);
    }

    #[test]
    fn parts_and_ranges() {
        assert_eq!(part_count(0, 4), 1);
        assert_eq!(part_count(8, 4), 2);
        assert_eq!(part_count(9, 4), 3);
        assert_eq!(part_range(1, 4, 9), (0, 4));
        assert_eq!(part_range(3, 4, 9), (8, 1));
        assert_eq!(part_range(1, 4, 0), (0, 0));
    }

    #[test]
    fn proof_range_matches_spec() {
        assert_eq!(proof_range("token", 0), None);
        // md5("abc") = 900150983cd24fb0d6963f7d28e17f72
        let h = u64::from_str_radix("900150983cd24fb0", 16).unwrap();
        for size in [1u64, 7, 8, 100, 1 << 40] {
            let start = h % size;
            assert_eq!(
                proof_range("abc", size),
                Some((start, (start + 8).min(size))),
                "size {size}"
            );
        }
    }

    #[tokio::test]
    async fn memory_source() {
        let data = b"hello world";
        let src = UploadSource::Memory(data);
        assert_eq!(src.size().await.unwrap(), 11);
        assert_eq!(src.sha1().await.unwrap(), "2AAE6C35C94FCFB415DBE95F408B9CE91EE846ED");
        assert_eq!(&src.read(6, 5).await.unwrap()[..], b"world");
        assert!(src.read(8, 5).await.is_err());
        assert_eq!(
            UploadSource::Memory(b"").sha1().await.unwrap(),
            "DA39A3EE5E6B4B0D3255BFEF95601890AFD80709"
        );
    }

    #[tokio::test]
    async fn file_source() {
        let path = std::env::temp_dir().join(format!("aliyunpan-src-{}", util::random_device_id().unwrap()));
        tokio::fs::write(&path, b"hello world").await.unwrap();
        let src = UploadSource::File(&path);
        assert_eq!(src.size().await.unwrap(), 11);
        assert_eq!(src.sha1().await.unwrap(), "2AAE6C35C94FCFB415DBE95F408B9CE91EE846ED");
        assert_eq!(&src.read(0, 5).await.unwrap()[..], b"hello");
        tokio::fs::remove_file(&path).await.unwrap();
    }

    #[test]
    fn state_progress() {
        let mut s = UploadState {
            drive_id: "d".into(),
            file_id: "f".into(),
            upload_id: "u".into(),
            file_name: "n".into(),
            chunk_size: 4,
            part_count: 3,
            completed_parts: BTreeSet::new(),
            local_size: 9,
            local_sha1: String::new(),
            urls: BTreeMap::new(),
        };
        s.completed_parts.extend([1, 3]);
        assert_eq!(s.uploaded_bytes(), 5);
        assert!(!s.is_complete());
        s.completed_parts.insert(2);
        assert!(s.is_complete());
        let json = serde_json::to_string(&s).unwrap();
        assert!(!json.contains("urls"));
        assert_eq!(serde_json::from_str::<UploadState>(&json).unwrap(), s);
    }
}
