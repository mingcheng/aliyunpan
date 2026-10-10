mod album;
mod batch;
mod bottle;
mod file;
mod recycle;
mod share;
mod user;

pub use batch::{BatchRequest, BatchResponse, BatchVersion};

pub(crate) const IMAGE_THUMBNAIL_PROCESS: &str = "image/resize,w_400/format,jpeg";
pub(crate) const IMAGE_URL_PROCESS: &str = "image/resize,w_1920/format,jpeg";
pub(crate) const VIDEO_THUMBNAIL_PROCESS: &str = "video/snapshot,t_0,f_jpg,ar_auto,w_800";

/// Include `marker` only when a pagination marker is present.
pub(crate) fn set_marker(body: &mut serde_json::Value, marker: Option<&str>) {
    if let Some(m) = marker.filter(|m| !m.is_empty()) {
        body["marker"] = m.into();
    }
}

pub(crate) fn ids<S: AsRef<str>>(ids: &[S]) -> Vec<&str> {
    ids.iter().map(AsRef::as_ref).collect()
}
