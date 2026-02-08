//! Configuration for the HumanSync node.
//!
//! This module provides the [`Config`] struct for configuring a HumanSync node.
//! Configuration includes storage paths, server URL, sync interval, and network settings.
//!
//! # Example
//!
//! ```rust
//! use humansync::Config;
//!
//! // Simple configuration with just a storage path
//! let config = Config::new("/path/to/storage");
//!
//! // Full configuration with builder pattern
//! let config = Config::new("/path/to/storage")
//!     .with_server_url("https://my.server.com")
//!     .with_sync_interval(60)  // sync every 60 seconds
//!     .with_iroh_port(5000);
//!
//! // Default configuration (uses platform-specific data directory)
//! let config = Config::default();
//! ```

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{DEFAULT_IROH_PORT, DEFAULT_SYNC_INTERVAL_SECS};

/// Configuration for the HumanSync node.
///
/// This struct configures all aspects of a HumanSync node including storage,
/// networking, and sync behavior. Use the builder-style methods to customize.
///
/// # Storage Layout
///
/// When initialized, HumanSync creates the following directory structure:
///
/// ```text
/// {storage_path}/
/// ├── device_key        # Ed25519 secret key (32 bytes)
/// ├── docs/             # Automerge documents
/// │   └── *.automerge
/// └── blobs/            # Content-addressed blob storage
///     └── exports/      # Exported blobs for reading
/// ```
///
/// # Defaults
///
/// - `server_url`: `None` (must be set for server sync)
/// - `storage_path`: Platform-specific data directory + "humansync"
/// - `sync_interval_secs`: 30 seconds
/// - `iroh_port`: 4433 (UDP)
/// - `relay_url`: `None` (uses n0 public relays)
/// - `max_blob_cache_bytes`: `None` (unlimited cache)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Server URL for pairing and peer discovery.
    ///
    /// This should be the base URL of your HumanSync server, e.g.,
    /// `https://my.server.com:8080`. Set to `None` for local-only mode.
    pub server_url: Option<String>,

    /// Path to store data (documents, blobs, identity).
    ///
    /// This directory will be created if it doesn't exist.
    pub storage_path: PathBuf,

    /// Sync interval in seconds.
    ///
    /// The background sync loop runs at this interval, syncing with all
    /// known peers. Default is 30 seconds.
    pub sync_interval_secs: u64,

    /// Iroh QUIC port.
    ///
    /// The UDP port that Iroh binds to for peer-to-peer connections.
    /// Default is 4433.
    pub iroh_port: u16,

    /// Optional self-hosted relay URL.
    ///
    /// If set, uses this relay server instead of n0's public relays.
    /// Format: `https://my-relay.example.com`
    pub relay_url: Option<String>,

    /// Maximum size in bytes for the exported blob cache.
    ///
    /// When set, the blob store will track access times for exported blobs
    /// and evict least-recently-used entries when the total cache size exceeds
    /// this limit. Set to `None` (the default) for unlimited cache size.
    pub max_blob_cache_bytes: Option<u64>,

    /// Human-readable name for this device.
    ///
    /// Used during QR/link pairing to identify the device to peers.
    /// Default is `"Device"`.
    pub device_name: String,
}

impl Config {
    /// Create a new configuration with the given storage path
    #[must_use]
    pub fn new(storage_path: impl Into<PathBuf>) -> Self {
        Self {
            server_url: None,
            storage_path: storage_path.into(),
            sync_interval_secs: DEFAULT_SYNC_INTERVAL_SECS,
            iroh_port: DEFAULT_IROH_PORT,
            relay_url: None,
            max_blob_cache_bytes: None,
            device_name: "Device".to_string(),
        }
    }

    /// Set the server URL
    #[must_use]
    pub fn with_server_url(mut self, url: impl Into<String>) -> Self {
        self.server_url = Some(url.into());
        self
    }

    /// Set the sync interval
    #[must_use]
    pub const fn with_sync_interval(mut self, secs: u64) -> Self {
        self.sync_interval_secs = secs;
        self
    }

    /// Set the Iroh port
    #[must_use]
    pub const fn with_iroh_port(mut self, port: u16) -> Self {
        self.iroh_port = port;
        self
    }

    /// Set the relay URL
    #[must_use]
    pub fn with_relay_url(mut self, url: impl Into<String>) -> Self {
        self.relay_url = Some(url.into());
        self
    }

    /// Set the device name
    #[must_use]
    pub fn with_device_name(mut self, name: impl Into<String>) -> Self {
        self.device_name = name.into();
        self
    }

    /// Set the maximum blob cache size in bytes.
    ///
    /// Controls LRU eviction of exported blob cache. When the total size of
    /// cached (exported) blobs exceeds this limit, the least-recently-used
    /// entries are evicted to bring the cache under the limit.
    #[must_use]
    pub fn with_max_blob_cache(mut self, bytes: u64) -> Self {
        self.max_blob_cache_bytes = Some(bytes);
        self
    }

    /// Get the default storage path
    #[must_use]
    pub fn default_storage_path() -> PathBuf {
        dirs::data_local_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("humansync")
    }

    /// Path to the device identity key
    #[must_use]
    pub fn identity_path(&self) -> PathBuf {
        self.storage_path.join("device_key")
    }

    /// Path to the documents directory
    #[must_use]
    pub fn docs_path(&self) -> PathBuf {
        self.storage_path.join("docs")
    }

    /// Path to the blobs directory
    #[must_use]
    pub fn blobs_path(&self) -> PathBuf {
        self.storage_path.join("blobs")
    }

    /// Path to the configuration file
    #[must_use]
    pub fn config_file_path(&self) -> PathBuf {
        self.storage_path.join("config.toml")
    }
}

impl Default for Config {
    fn default() -> Self {
        Self::new(Self::default_storage_path())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_default() {
        let config = Config::default();
        assert!(config.server_url.is_none());
        assert_eq!(config.sync_interval_secs, DEFAULT_SYNC_INTERVAL_SECS);
        assert_eq!(config.iroh_port, DEFAULT_IROH_PORT);
    }

    #[test]
    fn test_config_builder() {
        let config = Config::new("/tmp/test")
            .with_server_url("https://example.com")
            .with_sync_interval(60)
            .with_iroh_port(5000);

        assert_eq!(config.server_url, Some("https://example.com".to_string()));
        assert_eq!(config.sync_interval_secs, 60);
        assert_eq!(config.iroh_port, 5000);
    }

    #[test]
    fn test_config_blob_cache() {
        // Default should be None (unlimited)
        let config = Config::new("/tmp/test");
        assert!(config.max_blob_cache_bytes.is_none());

        // Builder should set the value
        let config = Config::new("/tmp/test")
            .with_max_blob_cache(1024 * 1024 * 100); // 100MB
        assert_eq!(config.max_blob_cache_bytes, Some(100 * 1024 * 1024));
    }

    #[test]
    fn test_config_paths() {
        let config = Config::new("/data/humansync");
        assert_eq!(config.identity_path(), PathBuf::from("/data/humansync/device_key"));
        assert_eq!(config.docs_path(), PathBuf::from("/data/humansync/docs"));
        assert_eq!(config.blobs_path(), PathBuf::from("/data/humansync/blobs"));
    }
}
