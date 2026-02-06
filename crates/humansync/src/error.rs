//! Error types for HumanSync.
//!
//! This module defines the [`enum@Error`] enum and [`Result`] type alias used throughout
//! the HumanSync library.
//!
//! # Error Categories
//!
//! Errors are categorized by the operation that failed:
//!
//! - [`Error::Init`] - Node initialization failures
//! - [`Error::Identity`] - Device identity/keypair issues
//! - [`Error::Connection`] - Peer connection failures
//! - [`Error::Sync`] - Document sync failures
//! - [`Error::Document`] - Document operations (put/get)
//! - [`Error::Blob`] - Blob storage operations
//! - [`Error::Storage`] - Filesystem operations
//! - [`Error::Pairing`] - Server pairing failures
//! - [`Error::Unauthorized`] - Authentication/authorization failures
//! - [`Error::Config`] - Configuration issues
//! - [`Error::Shutdown`] - Operation on shut down node
//!
//! # Example
//!
//! ```rust,no_run
//! use humansync::{HumanSync, Config, Error};
//!
//! # async fn example() -> humansync::Result<()> {
//! let node = HumanSync::init(Config::default()).await?;
//!
//! // Handle specific error types
//! match node.open_doc("test").await {
//!     Ok(doc) => println!("Opened document"),
//!     Err(Error::Storage(msg)) => eprintln!("Storage error: {}", msg),
//!     Err(Error::Shutdown) => eprintln!("Node is shut down"),
//!     Err(e) => eprintln!("Other error: {}", e),
//! }
//! # Ok(())
//! # }
//! ```

use std::sync::Arc;
use thiserror::Error;

/// Result type alias for HumanSync operations
pub type Result<T> = std::result::Result<T, Error>;

/// Errors that can occur in HumanSync operations
#[derive(Error, Debug, Clone)]
pub enum Error {
    /// Failed to initialize the sync node
    #[error("initialization failed: {0}")]
    Init(Arc<str>),

    /// Failed to load or save device identity
    #[error("identity error: {0}")]
    Identity(Arc<str>),

    /// Failed to connect to a peer
    #[error("connection failed: {0}")]
    Connection(Arc<str>),

    /// Failed to sync with a peer
    #[error("sync failed: {0}")]
    Sync(Arc<str>),

    /// Document operation failed
    #[error("document error: {0}")]
    Document(Arc<str>),

    /// Blob operation failed
    #[error("blob error: {0}")]
    Blob(Arc<str>),

    /// Storage operation failed
    #[error("storage error: {0}")]
    Storage(Arc<str>),

    /// Pairing failed
    #[error("pairing failed: {0}")]
    Pairing(Arc<str>),

    /// Device not authorized
    #[error("device not authorized: {0}")]
    Unauthorized(Arc<str>),

    /// Configuration error
    #[error("configuration error: {0}")]
    Config(Arc<str>),

    /// The node has been shut down
    #[error("node has been shut down")]
    Shutdown,
}

impl Error {
    /// Create an initialization error
    #[inline]
    pub fn init(msg: impl Into<String>) -> Self {
        Self::Init(Arc::from(msg.into()))
    }

    /// Create an identity error
    #[inline]
    pub fn identity(msg: impl Into<String>) -> Self {
        Self::Identity(Arc::from(msg.into()))
    }

    /// Create a connection error
    #[inline]
    pub fn connection(msg: impl Into<String>) -> Self {
        Self::Connection(Arc::from(msg.into()))
    }

    /// Create a sync error
    #[inline]
    pub fn sync(msg: impl Into<String>) -> Self {
        Self::Sync(Arc::from(msg.into()))
    }

    /// Create a document error
    #[inline]
    pub fn document(msg: impl Into<String>) -> Self {
        Self::Document(Arc::from(msg.into()))
    }

    /// Create a blob error
    #[inline]
    pub fn blob(msg: impl Into<String>) -> Self {
        Self::Blob(Arc::from(msg.into()))
    }

    /// Create a storage error
    #[inline]
    pub fn storage(msg: impl Into<String>) -> Self {
        Self::Storage(Arc::from(msg.into()))
    }

    /// Create a pairing error
    #[inline]
    pub fn pairing(msg: impl Into<String>) -> Self {
        Self::Pairing(Arc::from(msg.into()))
    }

    /// Create an unauthorized error
    #[inline]
    pub fn unauthorized(msg: impl Into<String>) -> Self {
        Self::Unauthorized(Arc::from(msg.into()))
    }

    /// Create a configuration error
    #[inline]
    pub fn config(msg: impl Into<String>) -> Self {
        Self::Config(Arc::from(msg.into()))
    }
}
