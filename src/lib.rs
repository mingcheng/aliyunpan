#![doc = include_str!("../README.md")]

mod api;
mod auth;
mod client;
mod config;
mod download;
mod error;
mod models;
mod path;
mod signature;
mod upload;
mod util;

pub use api::{BatchRequest, BatchResponse, BatchVersion};
pub use auth::{Credentials, FileStore, MemoryStore, TokenStore};
pub use bytes::Bytes;
pub use client::Client;
pub use config::{Config, DEFAULT_CHUNK_SIZE};
pub use download::{DEFAULT_URL_EXPIRE_SEC, DownloadStream};
pub use error::{ApiError, ApiErrorKind, Error, Result};
pub use models::*;
pub use path::{WalkEntry, Walker};
pub use signature::DeviceKey;
pub use upload::{
    CreateUpload, MAX_PARTS, UploadOptions, UploadOutcome, UploadSource, UploadStart, UploadState, chunk_size_for,
    part_count, part_range, proof_range,
};
pub use util::validate_file_name;
