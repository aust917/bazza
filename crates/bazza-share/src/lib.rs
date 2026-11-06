//! Sharing without a cloud: `.bazza` bundles (zip) for moving meetings between
//! computers, a standalone HTML export, and a small LAN web server for links.
//!
//! Disable the default `backend` feature for SQLite-free HTML rendering and
//! subtitle parsing. The default feature retains the bundle and server APIs.

#[cfg(feature = "backend")]
pub mod bundle;
pub mod html;
#[cfg(feature = "backend")]
pub mod server;
#[cfg(feature = "backend")]
pub mod staging;
pub mod subtitles;

#[cfg(feature = "backend")]
pub use bundle::{export_bundle, import_bundle, BundleManifest, ImportReport};
pub use html::render_standalone;
#[cfg(feature = "backend")]
pub use server::{ShareServer, ShareServerHandle};

#[derive(Debug, thiserror::Error)]
pub enum ShareError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[cfg(feature = "backend")]
    #[error("zip: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[cfg(feature = "backend")]
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[cfg(feature = "backend")]
    #[error("store: {0}")]
    Store(#[from] bazza_store::StoreError),
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, ShareError>;
