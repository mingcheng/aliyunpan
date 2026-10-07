use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::ids;
use crate::{
    client::{Client, Host},
    error::{ApiError, Error, Result},
    models::nullable,
};

/// Maximum subrequests per batch; a conservative value because the server limit is unknown.
const BATCH_LIMIT: usize = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BatchVersion {
    /// `/v2/batch`: add or remove favorites.
    V2,
    /// `/adrive/v2/batch`: save shared files and query async tasks.
    AdriveV2,
    /// `/adrive/v4/batch`: move, recycle, delete, and cancel shares.
    AdriveV4,
}

impl BatchVersion {
    fn path(self) -> &'static str {
        match self {
            Self::V2 => "/v2/batch",
            Self::AdriveV2 => "/adrive/v2/batch",
            Self::AdriveV4 => "/adrive/v4/batch",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct BatchRequest {
    pub id: String,
    pub method: String,
    pub url: String,
    pub headers: Value,
    pub body: Value,
}

impl BatchRequest {
    pub fn new(id: impl Into<String>, method: &str, url: &str, body: Value) -> Self {
        Self {
            id: id.into(),
            method: method.into(),
            url: url.into(),
            headers: json!({ "Content-Type": "application/json" }),
            body,
        }
    }
}

/// Subrequest result retaining the full body, including `async_task_id` and error details.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct BatchResponse {
    #[serde(deserialize_with = "nullable")]
    pub id: String,
    #[serde(deserialize_with = "nullable")]
    pub status: u16,
    pub body: Value,
}

impl BatchResponse {
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status) && ApiError::from_value(self.status, &self.body).is_none()
    }

    pub fn error(&self) -> Option<ApiError> {
        if let Some(e) = ApiError::from_value(self.status, &self.body) {
            return Some(e);
        }
        (!(200..300).contains(&self.status)).then(|| ApiError {
            code: format!("BatchStatus{}", self.status),
            message: self.body.to_string(),
            display_message: None,
            http_status: self.status,
        })
    }

    /// Task ID from a 202 async response.
    pub fn async_task_id(&self) -> Option<&str> {
        self.body
            .get("async_task_id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
    }

    pub fn into_result(self) -> Result<Self> {
        match self.error() {
            Some(e) => Err(Error::Api(e)),
            None => Ok(self),
        }
    }
}

#[derive(Deserialize)]
struct BatchEnvelope {
    #[serde(default, deserialize_with = "nullable")]
    responses: Vec<BatchResponse>,
}

impl Client {
    /// Send requests in batches of at most 100. Subrequest failures do not fail the overall call.
    pub async fn batch(
        &self,
        version: BatchVersion,
        requests: &[BatchRequest],
        share_token: Option<&str>,
    ) -> Result<Vec<BatchResponse>> {
        let extra: Vec<(&'static str, String)> = share_token
            .map(|t| vec![("x-share-token", t.to_owned())])
            .unwrap_or_default();
        let mut out = Vec::with_capacity(requests.len());
        for chunk in requests.chunks(BATCH_LIMIT) {
            let body = json!({ "requests": chunk, "resource": "file" });
            let env: BatchEnvelope = self.request(Host::Api, version.path(), &body, true, &extra).await?;
            out.extend(env.responses);
        }
        Ok(out)
    }

    fn file_batch<S: AsRef<str>>(drive_id: &str, file_ids: &[S], url: &str) -> Vec<BatchRequest> {
        ids(file_ids)
            .into_iter()
            .map(|id| BatchRequest::new(id, "POST", url, json!({ "drive_id": drive_id, "file_id": id })))
            .collect()
    }

    pub async fn move_files<S: AsRef<str>>(
        &self,
        drive_id: &str,
        file_ids: &[S],
        to_drive_id: &str,
        to_parent_file_id: &str,
    ) -> Result<Vec<BatchResponse>> {
        let reqs: Vec<_> = ids(file_ids)
            .into_iter()
            .map(|id| {
                let body = json!({
                    "drive_id": drive_id,
                    "file_id": id,
                    "to_drive_id": to_drive_id,
                    "to_parent_file_id": to_parent_file_id,
                });
                BatchRequest::new(id, "POST", "/file/move", body)
            })
            .collect();
        self.batch(BatchVersion::AdriveV4, &reqs, None).await
    }

    pub async fn trash<S: AsRef<str>>(&self, drive_id: &str, file_ids: &[S]) -> Result<Vec<BatchResponse>> {
        self.batch(
            BatchVersion::AdriveV4,
            &Self::file_batch(drive_id, file_ids, "/recyclebin/trash"),
            None,
        )
        .await
    }

    pub async fn restore<S: AsRef<str>>(&self, drive_id: &str, file_ids: &[S]) -> Result<Vec<BatchResponse>> {
        self.batch(
            BatchVersion::AdriveV4,
            &Self::file_batch(drive_id, file_ids, "/recyclebin/restore"),
            None,
        )
        .await
    }

    /// Permanently delete files that are already in the recycle bin.
    pub async fn delete_permanently<S: AsRef<str>>(
        &self,
        drive_id: &str,
        file_ids: &[S],
    ) -> Result<Vec<BatchResponse>> {
        self.batch(
            BatchVersion::AdriveV4,
            &Self::file_batch(drive_id, file_ids, "/file/delete"),
            None,
        )
        .await
    }

    /// Move files to the recycle bin, then permanently delete them; fail on any subrequest error.
    pub async fn purge<S: AsRef<str>>(&self, drive_id: &str, file_ids: &[S]) -> Result<Vec<BatchResponse>> {
        for r in self.trash(drive_id, file_ids).await? {
            r.into_result()?;
        }
        let deleted = self.delete_permanently(drive_id, file_ids).await?;
        for r in &deleted {
            if let Some(e) = r.error() {
                return Err(Error::Api(e));
            }
        }
        Ok(deleted)
    }

    pub async fn set_starred<S: AsRef<str>>(
        &self,
        drive_id: &str,
        file_ids: &[S],
        starred: bool,
    ) -> Result<Vec<BatchResponse>> {
        let index_key = if starred { "starred_yes" } else { "" };
        let reqs: Vec<_> = ids(file_ids)
            .into_iter()
            .map(|id| {
                let body = json!({
                    "drive_id": drive_id,
                    "file_id": id,
                    "starred": starred,
                    "custom_index_key": index_key,
                });
                BatchRequest::new(id, "PUT", "/file/update", body)
            })
            .collect();
        self.batch(BatchVersion::V2, &reqs, None).await
    }

    pub async fn star_files<S: AsRef<str>>(&self, drive_id: &str, file_ids: &[S]) -> Result<Vec<BatchResponse>> {
        self.set_starred(drive_id, file_ids, true).await
    }

    pub async fn unstar_files<S: AsRef<str>>(&self, drive_id: &str, file_ids: &[S]) -> Result<Vec<BatchResponse>> {
        self.set_starred(drive_id, file_ids, false).await
    }

    pub async fn cancel_share_links<S: AsRef<str>>(&self, share_ids: &[S]) -> Result<Vec<BatchResponse>> {
        let reqs: Vec<_> = ids(share_ids)
            .into_iter()
            .map(|id| BatchRequest::new(id, "POST", "/share_link/cancel", json!({ "share_id": id })))
            .collect();
        self.batch(BatchVersion::AdriveV4, &reqs, None).await
    }

    /// Save shared files to the current user's drive; not verified against the live API.
    pub async fn save_from_share<S: AsRef<str>>(
        &self,
        share_id: &str,
        share_token: &str,
        file_ids: &[S],
        to_drive_id: &str,
        to_parent_file_id: &str,
        auto_rename: bool,
    ) -> Result<Vec<BatchResponse>> {
        let reqs: Vec<_> = ids(file_ids)
            .into_iter()
            .map(|id| {
                let body = json!({
                    "share_id": share_id,
                    "file_id": id,
                    "auto_rename": auto_rename,
                    "to_drive_id": to_drive_id,
                    "to_parent_file_id": to_parent_file_id,
                });
                BatchRequest::new(id, "POST", "/file/copy", body)
            })
            .collect();
        self.batch(BatchVersion::AdriveV2, &reqs, Some(share_token)).await
    }

    /// Query async tasks in batches; not verified against the live API.
    pub async fn get_async_tasks<S: AsRef<str>>(
        &self,
        task_ids: &[S],
        share_token: Option<&str>,
    ) -> Result<Vec<BatchResponse>> {
        let reqs: Vec<_> = ids(task_ids)
            .into_iter()
            .map(|id| BatchRequest::new(id, "POST", "/async_task/get", json!({ "async_task_id": id })))
            .collect();
        self.batch(BatchVersion::AdriveV2, &reqs, share_token).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sub_response_status_and_body() {
        let ok: BatchResponse = serde_json::from_str(r#"{"id":"a","status":204}"#).unwrap();
        assert!(ok.is_success());
        assert!(ok.error().is_none());

        let pending: BatchResponse =
            serde_json::from_str(r#"{"id":"a","status":202,"body":{"async_task_id":"t1"}}"#).unwrap();
        assert_eq!(pending.async_task_id(), Some("t1"));

        let failed: BatchResponse =
            serde_json::from_str(r#"{"id":"b","status":404,"body":{"code":"NotFound.File","message":"gone"}}"#)
                .unwrap();
        assert!(!failed.is_success());
        assert!(failed.into_result().unwrap_err().is_not_found());

        let bare: BatchResponse = serde_json::from_str(r#"{"id":"c","status":500,"body":null}"#).unwrap();
        assert_eq!(bare.error().unwrap().code, "BatchStatus500");
    }

    #[test]
    fn request_shape() {
        let v = serde_json::to_value(BatchRequest::new("f1", "POST", "/file/move", json!({"x": 1}))).unwrap();
        assert_eq!(v["headers"]["Content-Type"], "application/json");
        assert_eq!(v["id"], "f1");
    }
}
