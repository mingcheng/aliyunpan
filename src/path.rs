use std::collections::VecDeque;

use crate::{
    client::Client,
    error::{Error, Result},
    models::{CheckNameMode, FileItem, FileKind, ListOptions, OrderBy, OrderDirection},
};

fn split_path(path: &str) -> Result<Vec<&str>> {
    if !path.starts_with('/') {
        return Err(Error::InvalidInput(format!("path must be absolute: {path:?}")));
    }
    Ok(path.split('/').filter(|s| !s.is_empty()).collect())
}

fn join(prefix: &str, name: &str) -> String {
    if prefix.is_empty() {
        name.to_owned()
    } else {
        format!("{prefix}/{name}")
    }
}

impl Client {
    /// List all children of a directory, fetching subsequent pages automatically.
    pub async fn list_all_files(&self, drive_id: &str, parent_file_id: &str) -> Result<Vec<FileItem>> {
        let mut opts = ListOptions::default().order(OrderBy::Name, OrderDirection::Asc);
        let mut out = Vec::new();
        loop {
            let page = self.list_files(drive_id, parent_file_id, &opts).await?;
            out.extend(page.items.iter().cloned());
            match page.next_marker() {
                Some(m) => {
                    opts.marker = Some(m.to_owned());
                    tokio::time::sleep(self.config().page_delay).await;
                }
                None => return Ok(out),
            }
        }
    }

    pub async fn find_child(&self, drive_id: &str, parent_file_id: &str, name: &str) -> Result<Option<FileItem>> {
        Ok(self
            .list_all_files(drive_id, parent_file_id)
            .await?
            .into_iter()
            .find(|i| i.name == name))
    }

    /// Resolve an absolute path by listing each directory; `/` returns a root placeholder.
    pub async fn get_by_path(&self, drive_id: &str, path: &str) -> Result<Option<FileItem>> {
        let mut current = FileItem::root(drive_id);
        for segment in split_path(path)? {
            if !current.is_folder() {
                return Ok(None);
            }
            match self.find_child(drive_id, &current.file_id, segment).await? {
                Some(item) => current = item,
                None => return Ok(None),
            }
        }
        Ok(Some(current))
    }

    /// Create directories recursively, reusing any that already exist.
    pub async fn mkdir_p(&self, drive_id: &str, path: &str) -> Result<FileItem> {
        let mut current = FileItem::root(drive_id);
        for segment in split_path(path)? {
            let existing = match self.find_child(drive_id, &current.file_id, segment).await? {
                Some(item) => Some(item),
                None => match self
                    .create_folder(drive_id, &current.file_id, segment, CheckNameMode::Refuse)
                    .await
                {
                    Ok(created) => {
                        current = FileItem {
                            drive_id: drive_id.into(),
                            file_id: created.file_id,
                            parent_file_id: current.file_id,
                            name: segment.into(),
                            kind: FileKind::Folder,
                            ..FileItem::default()
                        };
                        None
                    }
                    // Another request may have created the directory concurrently.
                    Err(e) if e.is_already_exists() => self.find_child(drive_id, &current.file_id, segment).await?,
                    Err(e) => return Err(e),
                },
            };
            if let Some(item) = existing {
                if !item.is_folder() {
                    return Err(Error::InvalidInput(format!("{segment:?} exists and is not a folder")));
                }
                current = item;
            }
        }
        Ok(current)
    }

    /// Walk recursively in breadth-first order, fetching pages without loading the entire tree.
    pub fn walk(&self, drive_id: &str, folder_id: &str) -> Walker {
        Walker {
            client: self.clone(),
            drive_id: drive_id.into(),
            pending: VecDeque::from([(folder_id.to_owned(), String::new())]),
            current: None,
            buffer: VecDeque::new(),
            fetched: false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct WalkEntry {
    /// Path relative to the traversal root, separated by `/`.
    pub path: String,
    pub item: FileItem,
}

#[derive(Debug)]
pub struct Walker {
    client: Client,
    drive_id: String,
    pending: VecDeque<(String, String)>,
    /// (folder_id, path_prefix, next_marker)
    current: Option<(String, String, Option<String>)>,
    buffer: VecDeque<WalkEntry>,
    fetched: bool,
}

impl Walker {
    /// Return the next entry; traversal stops after an error.
    pub async fn next(&mut self) -> Option<Result<WalkEntry>> {
        loop {
            if let Some(entry) = self.buffer.pop_front() {
                if entry.item.is_folder() {
                    self.pending.push_back((entry.item.file_id.clone(), entry.path.clone()));
                }
                return Some(Ok(entry));
            }
            let (folder, prefix, marker) = match self.current.take() {
                Some((f, p, Some(m))) => (f, p, Some(m)),
                _ => {
                    let (f, p) = self.pending.pop_front()?;
                    (f, p, None)
                }
            };
            if self.fetched {
                tokio::time::sleep(self.client.config().page_delay).await;
            }
            self.fetched = true;
            let opts = ListOptions {
                marker,
                ..ListOptions::default().order(OrderBy::Name, OrderDirection::Asc)
            };
            match self.client.list_files(&self.drive_id, &folder, &opts).await {
                Ok(page) => {
                    let next = page.next_marker().map(str::to_owned);
                    self.buffer.extend(page.items.into_iter().map(|item| WalkEntry {
                        path: join(&prefix, &item.name),
                        item,
                    }));
                    self.current = Some((folder, prefix, next));
                }
                Err(e) => {
                    self.pending.clear();
                    return Some(Err(e));
                }
            }
        }
    }

    /// Collect all remaining entries.
    pub async fn collect(mut self) -> Result<Vec<WalkEntry>> {
        let mut out = Vec::new();
        while let Some(entry) = self.next().await {
            out.push(entry?);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_splitting() {
        assert_eq!(split_path("/").unwrap(), Vec::<&str>::new());
        assert_eq!(split_path("/a//b/").unwrap(), vec!["a", "b"]);
        assert!(split_path("a/b").is_err());
        assert_eq!(join("", "a"), "a");
        assert_eq!(join("a", "b"), "a/b");
    }
}
