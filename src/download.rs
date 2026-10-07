use std::{ops::Range, path::Path};

use bytes::Bytes;
use serde_json::json;
use sha1::{Digest, Sha1};
use tokio::io::AsyncWriteExt;

use crate::{
    client::Client,
    error::{Error, Result},
    models::{DownloadUrl, FileItem},
    util,
};

/// Default download URL lifetime: four hours.
pub const DEFAULT_URL_EXPIRE_SEC: u32 = 14_400;

/// Download response body that can be consumed one chunk at a time.
#[derive(Debug)]
pub struct DownloadStream {
    resp: reqwest::Response,
}

impl DownloadStream {
    pub fn status(&self) -> u16 {
        self.resp.status().as_u16()
    }

    pub fn content_length(&self) -> Option<u64> {
        self.resp.content_length()
    }

    pub async fn chunk(&mut self) -> Result<Option<Bytes>> {
        Ok(self.resp.chunk().await?)
    }

    pub async fn bytes(self) -> Result<Bytes> {
        Ok(self.resp.bytes().await?)
    }
}

impl Client {
    pub async fn get_download_url(&self, drive_id: &str, file_id: &str, expire_sec: u32) -> Result<DownloadUrl> {
        let body = json!({ "drive_id": drive_id, "file_id": file_id, "expire_sec": expire_sec });
        self.post("/v2/file/get_download_url", &body).await
    }

    /// GET a download URL with an optional half-open byte range. Only 200 and 206 are successful.
    pub async fn open_download_url(&self, url: &str, range: Option<Range<u64>>) -> Result<DownloadStream> {
        let range_header = match range {
            Some(r) if r.start >= r.end => return Err(Error::InvalidInput(format!("empty range {r:?}"))),
            Some(r) => Some(format!("bytes={}-{}", r.start, r.end - 1)),
            None => None,
        };
        let referer = &self.config().download_referer;
        let resp = self
            .send_transfer(|http| {
                let req = http.get(url).header("referer", referer.as_str());
                match &range_header {
                    Some(h) => req.header("range", h.as_str()),
                    None => req,
                }
            })
            .await?;
        let status = resp.status().as_u16();
        match status {
            200 | 206 => Ok(DownloadStream { resp }),
            429 | 509 => Err(Error::RateLimited {
                status,
                retry_after: None,
            }),
            _ => Err(Error::Http {
                status,
                body: resp.text().await.unwrap_or_default(),
            }),
        }
    }

    /// Fetch a download URL and open it, renewing the URL once on 403 (expiration).
    pub async fn download(&self, drive_id: &str, file_id: &str, range: Option<Range<u64>>) -> Result<DownloadStream> {
        let mut retried = false;
        loop {
            let url = self.get_download_url(drive_id, file_id, DEFAULT_URL_EXPIRE_SEC).await?;
            if url.is_blocked() {
                return Err(Error::Blocked);
            }
            match self.open_download_url(&url.url, range.clone()).await {
                Err(Error::Http { status: 403, .. }) if !retried => retried = true,
                other => return other,
            }
        }
    }

    /// Download and verify size and available SHA1, writing to `<dest>.part` before renaming on success.
    pub async fn download_to_file(&self, drive_id: &str, file_id: &str, dest: &Path) -> Result<FileItem> {
        let meta = self.get_file(drive_id, file_id).await?;
        if !meta.is_file() {
            return Err(Error::InvalidInput(format!("{} is not a file", meta.name)));
        }
        let mut tmp = dest.as_os_str().to_owned();
        tmp.push(".part");
        let tmp = std::path::PathBuf::from(tmp);

        let result = async {
            let mut out = tokio::fs::File::create(&tmp).await?;
            let mut hasher = Sha1::new();
            let mut written = 0u64;
            if meta.size > 0 {
                let mut stream = self.download(drive_id, file_id, None).await?;
                while let Some(chunk) = stream.chunk().await? {
                    hasher.update(&chunk);
                    out.write_all(&chunk).await?;
                    written += chunk.len() as u64;
                }
            }
            out.flush().await?;
            out.sync_all().await?;
            if written != meta.size {
                return Err(Error::Integrity {
                    expected: format!("{} bytes", meta.size),
                    actual: format!("{written} bytes"),
                });
            }
            let actual = util::hex_upper(&hasher.finalize());
            if let Some(expected) = meta
                .content_hash
                .as_deref()
                .filter(|hash| !hash.is_empty() && !meta.sha1_matches(&actual))
            {
                return Err(Error::Integrity {
                    expected: expected.to_owned(),
                    actual,
                });
            }
            Ok(())
        }
        .await;

        match result {
            Ok(()) => {
                tokio::fs::rename(&tmp, dest).await?;
                Ok(meta)
            }
            Err(e) => {
                let _ = tokio::fs::remove_file(&tmp).await;
                Err(e)
            }
        }
    }
}
