use std::time::Duration;

use serde_json::json;

use super::{IMAGE_THUMBNAIL_PROCESS, VIDEO_THUMBNAIL_PROCESS, set_marker};
use crate::{
    client::Client,
    error::{Error, Result},
    models::{AsyncTask, AsyncTaskRef, FileItem, ListOptions, Page},
};

impl Client {
    pub async fn list_recycle_bin(&self, drive_id: &str, opts: &ListOptions) -> Result<Page<FileItem>> {
        let mut body = json!({
            "drive_id": drive_id,
            "limit": opts.limit,
            "image_thumbnail_process": IMAGE_THUMBNAIL_PROCESS,
            "video_thumbnail_process": VIDEO_THUMBNAIL_PROCESS,
            "order_by": opts.order_by,
            "order_direction": opts.order_direction,
        });
        set_marker(&mut body, opts.marker.as_deref());
        self.post("/adrive/v2/recyclebin/list", &body).await
    }

    /// List all recycle bin entries, fetching subsequent pages automatically.
    pub async fn list_all_recycle_bin(&self, drive_id: &str, opts: &ListOptions) -> Result<Vec<FileItem>> {
        let mut opts = opts.clone();
        let mut out = Vec::new();
        loop {
            let page = self.list_recycle_bin(drive_id, &opts).await?;
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

    /// Empty the entire recycle bin. Irreversible and not verified against the live API.
    pub async fn clear_recycle_bin(&self, drive_id: &str) -> Result<AsyncTaskRef> {
        self.post("/v2/recyclebin/clear", &json!({ "drive_id": drive_id }))
            .await
    }

    /// Query an async task; not verified against the live API.
    pub async fn get_async_task(&self, async_task_id: &str) -> Result<AsyncTask> {
        self.post("/v2/async_task/get", &json!({ "async_task_id": async_task_id }))
            .await
    }

    /// Poll until an async task succeeds or fails.
    pub async fn wait_async_task(
        &self,
        async_task_id: &str,
        poll_interval: Duration,
        timeout: Duration,
    ) -> Result<AsyncTask> {
        if async_task_id.is_empty() {
            return Err(Error::InvalidInput("async_task_id is empty".into()));
        }
        let start = tokio::time::Instant::now();
        let poll = poll_interval.max(Duration::from_millis(100));
        loop {
            let task = self.get_async_task(async_task_id).await?;
            if task.is_finished() {
                return Ok(task);
            }
            if start.elapsed() >= timeout {
                return Err(Error::InvalidInput(format!(
                    "async task {async_task_id} did not finish within {:.3}s",
                    timeout.as_secs_f64()
                )));
            }
            tokio::time::sleep(poll).await;
        }
    }

    /// Empty the recycle bin and wait for the async task to finish.
    pub async fn clear_recycle_bin_and_wait(
        &self,
        drive_id: &str,
        poll_interval: Duration,
        timeout: Duration,
    ) -> Result<AsyncTask> {
        let task = self.clear_recycle_bin(drive_id).await?;
        let id = task
            .async_task_id
            .or(task.task_id)
            .filter(|id| !id.is_empty())
            .ok_or_else(|| Error::InvalidInput("clear_recycle_bin response lacks async_task_id".into()))?;
        self.wait_async_task(&id, poll_interval, timeout).await
    }
}
