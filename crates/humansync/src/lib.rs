//! # HumanSync - Local-first sync layer for HumanCorp apps
//!
//! HumanSync is a self-hosted, local-first sync layer that gives users an iCloud-like
//! experience across all their devices without depending on any third-party cloud.
//! The device is always the source of truth; the server is just another peer that
//! happens to have good uptime.
//!
//! ## Features
//!
//! - **Offline-first**: All reads and writes are local and instant. No loading spinners.
//! - **Automatic conflict resolution**: CRDTs (Automerge) handle concurrent edits seamlessly.
//! - **Peer-to-peer sync**: Devices sync directly via Iroh when on the same network.
//! - **Large file support**: Binary files handled separately via iroh-blobs.
//! - **Self-hosted**: Deploy your own server with a single Docker container.
//!
//! ## Architecture
//!
//! HumanSync is built on three core technologies:
//!
//! - **[Automerge](https://automerge.org/)**: CRDT-based documents for conflict-free merging
//! - **[iroh-blobs](https://iroh.computer/)**: Content-addressed storage for large files
//! - **[Iroh](https://iroh.computer/)**: P2P transport with NAT traversal and relay fallback
//!
//! ## Quick Start
//!
//! ```rust,no_run
//! use humansync::{HumanSync, Config};
//! use std::path::Path;
//!
//! #[tokio::main]
//! async fn main() -> humansync::Result<()> {
//!     // Initialize the sync node with custom storage path
//!     let config = Config::new("/path/to/storage")
//!         .with_server_url("https://my.server.com");
//!     let node = HumanSync::init(config).await?;
//!
//!     // Pair with server (first time only)
//!     // node.pair("https://my.server.com", "server-password").await?;
//!
//!     // Open or create a document
//!     let doc = node.open_doc("notes/shopping-list").await?;
//!
//!     // Read/write - always local, always instant
//!     doc.put("eggs", 12)?;
//!     let eggs: Option<i64> = doc.get("eggs")?;
//!     assert_eq!(eggs, Some(12));
//!
//!     // Store a large file as a blob
//!     let blob_hash = node.store_blob(Path::new("photo.jpg")).await?;
//!
//!     // Reference the blob from a document
//!     doc.put("attachment", &blob_hash)?;
//!
//!     // Sync status (for UI indicators)
//!     let status = node.sync_status();
//!     println!("Connected peers: {}", status.peers_connected);
//!
//!     // Graceful shutdown
//!     node.shutdown().await?;
//!     Ok(())
//! }
//! ```
//!
//! ## Module Overview
//!
//! - [`config`]: Configuration for the HumanSync node
//! - [`doc`]: Document operations (put/get/delete)
//! - [`error`]: Error types and Result alias
//! - [`node`]: Main [`HumanSync`] node struct
//! - [`registry`]: Device registry for peer authentication
//! - [`store`]: Local persistence for documents and blobs
//!
//! ## Internal Modules
//!
//! The following modules are used internally and are not part of the stable API:
//!
//! - [`identity`]: Device identity (keypair) management
//! - [`sync`]: Sync protocol implementation

#![forbid(unsafe_code)]
#![warn(missing_docs, clippy::all, clippy::pedantic)]

// =============================================================================
// Public modules - stable API
// =============================================================================

pub mod config;
pub mod doc;
pub mod error;
pub mod node;
pub mod registry;
pub mod store;

// =============================================================================
// Internal modules - not part of stable API
// =============================================================================

#[doc(hidden)]
pub mod identity;

#[doc(hidden)]
pub mod sync;

// =============================================================================
// Public re-exports - the primary public API
// =============================================================================

pub use config::Config;
pub use doc::{CursorInfo, Document};
pub use error::{Error, Result};
pub use iroh::NodeId;
pub use node::{HumanSync, SyncStatus};
pub use registry::{DeviceInfo, DeviceRegistry, DeviceRegistryStore, DEVICES_DOC_NAME};

// =============================================================================
// Constants
// =============================================================================

/// The ALPN (Application-Layer Protocol Negotiation) identifier for HumanSync.
///
/// This is used during the QUIC handshake to identify the HumanSync protocol.
pub const ALPN: &[u8] = b"humansync/1";

/// Default sync interval in seconds.
///
/// The background sync loop will attempt to sync with peers at this interval.
/// Can be configured via [`Config::with_sync_interval`].
pub const DEFAULT_SYNC_INTERVAL_SECS: u64 = 1;

/// Default Iroh QUIC port.
///
/// The Iroh endpoint will bind to this UDP port by default.
/// Can be configured via [`Config::with_iroh_port`].
pub const DEFAULT_IROH_PORT: u16 = 4433;
