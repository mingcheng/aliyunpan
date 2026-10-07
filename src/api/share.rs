use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use super::{ids, set_marker};
use crate::{
    client::{Client, Host},
    error::{Error, Result, unexpected_response},
    models::{AnonymousShare, DriveFile, FastShare, ListOptions, Page, ShareLink, ShareToken, SharedFile},
};

impl Client {
    /// Extract a share ID from a share URL or a raw `share_id`.
    pub fn extract_share_id(input: &str) -> Option<String> {
        extract_share_id(input)
    }

    /// Create a share. An empty `share_pwd` makes it public; `expiration: None` disables expiration.
    /// Live testing returned a server message requiring an upgrade to use this feature.
    pub async fn create_share_link<S: AsRef<str>>(
        &self,
        drive_id: &str,
        file_ids: &[S],
        share_pwd: &str,
        expiration: Option<&str>,
    ) -> Result<ShareLink> {
        let body = json!({
            "drive_id": drive_id,
            "share_pwd": share_pwd,
            "expiration": expiration.unwrap_or_default(),
            "file_id_list": ids(file_ids),
        });
        let value = self.post("/adrive/v2/share_link/create", &body).await?;
        decode_created_share("create_share_link", value)
    }

    pub async fn list_share_links(&self, opts: &ListOptions) -> Result<Page<ShareLink>> {
        let mut body = json!({
            "category": "file,album",
            "creator": self.user_id().await,
            "include_canceled": false,
            "order_by": opts.order_by,
            "order_direction": opts.order_direction,
            "limit": opts.limit,
        });
        set_marker(&mut body, opts.marker.as_deref());
        self.post("/adrive/v3/share_link/list", &body).await
    }

    /// List all shares, fetching subsequent pages automatically.
    pub async fn list_all_share_links(&self, opts: &ListOptions) -> Result<Vec<ShareLink>> {
        let mut opts = opts.clone();
        let mut out = Vec::new();
        loop {
            let page = self.list_share_links(&opts).await?;
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

    /// Not verified against the live API.
    pub async fn get_share_by_anonymous(&self, share_id: &str) -> Result<AnonymousShare> {
        let path = format!(
            "/adrive/v3/share_link/get_share_by_anonymous?share_id={}",
            query_escape(share_id)
        );
        self.post(&path, &json!({ "share_id": share_id })).await
    }

    /// Not verified against the live API.
    pub async fn get_share_token(&self, share_id: &str, share_pwd: &str) -> Result<ShareToken> {
        self.post(
            "/v2/share_link/get_share_token",
            &json!({ "share_id": share_id, "share_pwd": share_pwd }),
        )
        .await
    }

    /// Obtain a share token from a share URL or share ID.
    pub async fn get_share_token_by_url(&self, share_url_or_id: &str, share_pwd: &str) -> Result<ShareToken> {
        let share_id = extract_share_id(share_url_or_id)
            .ok_or_else(|| crate::Error::InvalidInput(format!("invalid share id or url: {share_url_or_id}")))?;
        self.get_share_token(&share_id, share_pwd).await
    }

    /// List files in a share; not verified against the live API.
    pub async fn list_shared_files(
        &self,
        share_id: &str,
        share_token: &str,
        parent_file_id: &str,
        opts: &ListOptions,
    ) -> Result<Page<SharedFile>> {
        let mut body = json!({
            "share_id": share_id,
            "parent_file_id": parent_file_id,
            "limit": opts.limit,
            "image_thumbnail_process": "image/resize,w_256/format,jpeg",
            "image_url_process": "image/resize,w_1920/format,jpeg/interlace,1",
            "video_thumbnail_process": "video/snapshot,t_1000,f_jpg,ar_auto,w_256",
            "order_by": opts.order_by,
            "order_direction": opts.order_direction,
        });
        set_marker(&mut body, opts.marker.as_deref());
        let extra = [("x-share-token", share_token.to_owned())];
        self.request(Host::Api, "/adrive/v2/file/list_by_share", &body, true, &extra)
            .await
    }

    /// List all files in a share, fetching subsequent pages automatically.
    pub async fn list_all_shared_files(
        &self,
        share_id: &str,
        share_token: &str,
        parent_file_id: &str,
        opts: &ListOptions,
    ) -> Result<Vec<SharedFile>> {
        let mut opts = opts.clone();
        let mut out = Vec::new();
        loop {
            let page = self
                .list_shared_files(share_id, share_token, parent_file_id, &opts)
                .await?;
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

    /// Create a quick-transfer link that expires after about 24 hours and cannot be canceled manually.
    pub async fn create_fast_share(&self, files: &[DriveFile]) -> Result<FastShare> {
        let value = self
            .post("/adrive/v1/share/create", &json!({ "drive_file_list": files }))
            .await?;
        decode_created_share("create_fast_share", value)
    }

    /// Create a quick transfer from a list of `(drive_id, file_id)` pairs.
    pub async fn create_fast_share_from_ids<S: AsRef<str>>(&self, drive_id: &str, file_ids: &[S]) -> Result<FastShare> {
        let files: Vec<_> = ids(file_ids)
            .into_iter()
            .map(|id| DriveFile::new(drive_id, id))
            .collect();
        self.create_fast_share(&files).await
    }
}

pub(super) fn decode_created_share<T: DeserializeOwned>(operation: &'static str, value: Value) -> Result<T> {
    if !value
        .get("share_id")
        .and_then(Value::as_str)
        .is_some_and(|id| !id.trim().is_empty())
    {
        return Err(unexpected_response(operation, &value));
    }
    serde_json::from_value(value.clone()).map_err(|source| Error::Decode {
        source,
        body: value.to_string(),
    })
}

fn query_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn extract_share_id(input: &str) -> Option<String> {
    let s = input.trim();
    if s.is_empty() {
        return None;
    }
    for needle in ["share_id=", "/s/"] {
        if let Some((_, tail)) = s.split_once(needle) {
            let id = tail.split(['?', '#', '&', '/']).next().unwrap_or_default();
            if !id.is_empty() {
                return Some(id.to_owned());
            }
        }
    }
    let id = s.split(['?', '#', '&', '/']).next().unwrap_or_default();
    (!id.is_empty()).then(|| id.to_owned())
}

#[cfg(test)]
mod tests {
    #[test]
    fn query_escape() {
        assert_eq!(super::query_escape("abc-_.~"), "abc-_.~");
        assert_eq!(super::query_escape("a b&c=d"), "a%20b%26c%3Dd");
    }

    #[test]
    fn share_id_extraction() {
        assert_eq!(
            super::extract_share_id("https://www.alipan.com/s/AbC123?pwd=0000"),
            Some("AbC123".into())
        );
        assert_eq!(
            super::extract_share_id("https://www.alipan.com/?share_id=xyz-7&from=web"),
            Some("xyz-7".into())
        );
        assert_eq!(super::extract_share_id("raw_share_id"), Some("raw_share_id".into()));
        assert_eq!(super::extract_share_id(""), None);
    }
}
