//! Main HumanSync node implementation.
//!
//! This module contains the [`HumanSync`] struct, which is the primary interface
//! for applications using HumanSync. It manages:
//!
//! - **Device identity**: Persistent Iroh keypair for this device
//! - **Document storage**: Local persistence and retrieval of Automerge documents
//! - **Blob storage**: Large file storage via iroh-blobs
//! - **Background sync**: Automatic synchronization with peers
//! - **Server connection**: Pairing and discovery via HumanSync server
//!
//! # Example
//!
//! ```rust,no_run
//! use humansync::{HumanSync, Config};
//! use std::path::Path;
//!
//! # async fn example() -> humansync::Result<()> {
//! // Initialize with default configuration
//! let node = HumanSync::init(Config::default()).await?;
//!
//! // Work with documents
//! let doc = node.open_doc("my-document").await?;
//! doc.put("key", "value")?;
//!
//! // Store a blob
//! let hash = node.store_blob(Path::new("photo.jpg")).await?;
//! doc.put("photo", &hash)?;
//!
//! // Check sync status
//! let status = node.sync_status();
//! println!("Peers: {}", status.peers_connected);
//!
//! // Trigger immediate sync (normally happens automatically)
//! node.sync_now().await?;
//!
//! // Shutdown when done
//! node.shutdown().await?;
//! # Ok(())
//! # }
//! ```

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use iroh::{Endpoint, NodeAddr, NodeId, SecretKey};
use parking_lot::RwLock;
use sha2::{Digest, Sha256};
use tokio::sync::{oneshot, watch};
use tokio::task::JoinHandle;
use tracing::{debug, error, info, warn};

/// Number of consecutive sync failures before a peer is automatically pruned.
const MAX_CONSECUTIVE_FAILURES: u32 = 120;

/// Tag byte for the pairing handshake protocol.
const PAIR_REQUEST_TAG: u8 = 0xFE;

/// Information returned when entering pairing mode, used to generate QR/link.
#[derive(Debug, Clone, serde::Serialize)]
pub struct PairingInfo {
    /// This device's node ID as a hex string.
    pub node_id: String,
    /// This device's human-readable name.
    pub device_name: String,
}

/// Result of a successful pairing handshake.
#[derive(Debug, Clone, serde::Serialize)]
pub struct PairingResult {
    /// The remote device's node ID as a hex string.
    pub node_id: String,
    /// The remote device's human-readable name.
    pub device_name: String,
}

use crate::config::Config;
use crate::doc::Document;
use crate::error::{Error, Result};
use crate::identity::load_or_create_identity;
use crate::registry::DeviceRegistryStore;
use crate::store::Store;
use crate::sync::automerge::{request_doc_list, sync_docs};
use crate::sync::{handle_blob_request, handle_doc_list_request, run_sync_protocol};
use crate::ALPN;

/// Status information about the sync state.
///
/// This struct provides visibility into the current sync status for UI indicators
/// or debugging. It is obtained via [`HumanSync::sync_status()`].
///
/// # Example
///
/// ```rust,no_run
/// # use humansync::{HumanSync, Config};
/// # async fn example() -> humansync::Result<()> {
/// let node = HumanSync::init(Config::default()).await?;
/// let status = node.sync_status();
///
/// println!("Connected to {} peers", status.peers_connected);
/// if let Some(last) = status.last_sync {
///     println!("Last synced at: {}", last);
/// }
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone, Default)]
pub struct SyncStatus {
    /// Number of peers currently connected and syncing.
    ///
    /// This reflects the peers we successfully synced with in the last sync cycle.
    pub peers_connected: usize,

    /// Number of documents with pending changes waiting to sync.
    ///
    /// This is reset to 0 after each successful sync cycle.
    pub docs_pending: usize,

    /// Timestamp of the last successful sync cycle.
    ///
    /// `None` if no sync has completed yet since node initialization.
    pub last_sync: Option<chrono::DateTime<chrono::Utc>>,
}

/// Internal state of the node
enum NodeState {
    /// Node is running
    Running {
        endpoint: Endpoint,
        secret_key: SecretKey,
        /// Handle to the background sync loop task
        sync_loop_handle: Option<JoinHandle<()>>,
        /// Sender to signal shutdown to the sync loop
        shutdown_tx: watch::Sender<bool>,
    },
    /// Node has been shut down
    Shutdown,
}

impl std::fmt::Debug for NodeState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Running { endpoint, secret_key, .. } => f
                .debug_struct("Running")
                .field("endpoint", endpoint)
                .field("secret_key", secret_key)
                .finish(),
            Self::Shutdown => write!(f, "Shutdown"),
        }
    }
}

/// The main HumanSync node.
///
/// This is the primary interface for applications using HumanSync.
/// It manages the Iroh endpoint, document store, device registry, and
/// background sync loop.
///
/// # Lifecycle
///
/// 1. Create with [`HumanSync::init()`] - initializes identity, storage, and starts sync loop
/// 2. Pair with server using [`HumanSync::pair()`] (first time only)
/// 3. Use documents with [`HumanSync::open_doc()`], [`HumanSync::list_docs()`]
/// 4. Use blobs with [`HumanSync::store_blob()`], [`HumanSync::get_blob()`]
/// 5. Shut down with [`HumanSync::shutdown()`]
///
/// # Thread Safety
///
/// `HumanSync` is designed to be shared across async tasks. The internal state
/// is protected by locks, so multiple tasks can safely access the node concurrently.
///
/// # Example
///
/// ```rust,no_run
/// use humansync::{HumanSync, Config};
///
/// # async fn example() -> humansync::Result<()> {
/// let config = Config::new("~/.humansync")
///     .with_server_url("https://my.server.com");
///
/// let node = HumanSync::init(config).await?;
///
/// // Open or create documents
/// let doc = node.open_doc("notes/shopping-list").await?;
/// doc.put("eggs", 12)?;
///
/// // Sync runs automatically in the background
/// // Check status anytime:
/// let status = node.sync_status();
///
/// // Graceful shutdown
/// node.shutdown().await?;
/// # Ok(())
/// # }
/// ```
pub struct HumanSync {
    /// Configuration
    config: Config,
    /// Internal state
    state: Arc<RwLock<NodeState>>,
    /// Document and blob store
    store: Arc<Store>,
    /// Device registry (persistent, backed by `_devices` Automerge doc)
    registry: Arc<DeviceRegistryStore>,
    /// Sync status
    sync_status: Arc<RwLock<SyncStatus>>,
    /// Whether the node is currently in pairing mode (accepting unknown peers).
    pairing_mode: Arc<AtomicBool>,
    /// One-shot sender to deliver the pairing result when a peer pairs successfully.
    pairing_result_tx: Arc<RwLock<Option<oneshot::Sender<PairingResult>>>>,
    /// SHA-256 hash of the PIN, set when entering pairing mode.
    pairing_pin_hash: Arc<RwLock<Option<[u8; 32]>>>,
}

impl HumanSync {
    /// Initialize a new HumanSync node
    ///
    /// This will:
    /// 1. Load or create a persistent device identity
    /// 2. Initialize the Iroh endpoint
    /// 3. Start the background sync loop
    pub async fn init(config: Config) -> Result<Self> {
        info!("Initializing HumanSync node");

        // Ensure storage directories exist
        tokio::fs::create_dir_all(&config.storage_path)
            .await
            .map_err(|e| Error::init(format!("failed to create storage directory: {e}")))?;

        // Load or create device identity
        let secret_key = load_or_create_identity(&config.identity_path()).await?;
        let node_id = secret_key.public();
        info!(node_id = %node_id, "Device identity ready");

        // Initialize Iroh endpoint
        let endpoint = Endpoint::builder()
            .secret_key(secret_key.clone())
            .alpns(vec![ALPN.to_vec()])
            .discovery_n0()
            .discovery_local_network()
            .bind()
            .await
            .map_err(|e| Error::init(format!("failed to bind Iroh endpoint: {e}")))?;

        debug!("Iroh endpoint initialized");

        // Initialize store
        let store = Arc::new(Store::new(&config)?);

        // Initialize the blob store (iroh-blobs)
        store
            .init_blob_store()
            .await
            .map_err(|e| Error::init(format!("failed to initialize blob store: {e}")))?;
        debug!("Blob store initialized");

        // Initialize device registry (persistent, loads _devices doc from disk if exists)
        let registry = Arc::new(
            DeviceRegistryStore::new(&config.storage_path)
                .map_err(|e| Error::init(format!("failed to initialize device registry: {e}")))?,
        );

        // Create shutdown signal channel
        let (shutdown_tx, shutdown_rx) = watch::channel(false);

        let sync_status = Arc::new(RwLock::new(SyncStatus::default()));

        let pairing_mode = Arc::new(AtomicBool::new(false));
        let pairing_result_tx = Arc::new(RwLock::new(None));
        let pairing_pin_hash: Arc<RwLock<Option<[u8; 32]>>> = Arc::new(RwLock::new(None));

        let node = Self {
            config: config.clone(),
            state: Arc::new(RwLock::new(NodeState::Running {
                endpoint: endpoint.clone(),
                secret_key,
                sync_loop_handle: None,
                shutdown_tx,
            })),
            store: store.clone(),
            registry: registry.clone(),
            sync_status: sync_status.clone(),
            pairing_mode: pairing_mode.clone(),
            pairing_result_tx: pairing_result_tx.clone(),
            pairing_pin_hash: pairing_pin_hash.clone(),
        };

        // Start accept loop for incoming P2P connections
        Self::start_accept_loop(
            endpoint.clone(),
            store.clone(),
            registry.clone(),
            pairing_mode,
            pairing_result_tx,
            pairing_pin_hash,
            config.device_name.clone(),
            sync_status.clone(),
        );

        // Start background sync loop
        let sync_loop_handle = Self::start_sync_loop(
            endpoint,
            store,
            registry,
            sync_status,
            config.sync_interval_secs,
            shutdown_rx,
        );

        // Update state with the sync loop handle
        {
            let mut state = node.state.write();
            if let NodeState::Running { sync_loop_handle: handle, .. } = &mut *state {
                *handle = Some(sync_loop_handle);
            }
        }

        info!("HumanSync node initialized successfully");
        Ok(node)
    }

    /// Get this device's NodeId
    pub fn node_id(&self) -> Result<NodeId> {
        let state = self.state.read();
        match &*state {
            NodeState::Running { secret_key, .. } => Ok(secret_key.public()),
            NodeState::Shutdown => Err(Error::Shutdown),
        }
    }

    /// Pair this device with a HumanSync server
    ///
    /// This registers this device's NodeId with the server and
    /// receives the list of other registered devices.
    pub async fn pair(&self, server_url: &str, password: &str) -> Result<()> {
        let node_id = self.node_id()?;
        info!(server_url, node_id = %node_id, "Pairing device with server");

        let url = format!("{}/pair", server_url.trim_end_matches('/'));

        let body = PairRequestBody {
            node_id: node_id.to_string(),
            device_name: "Device".to_string(),
            password: password.to_string(),
        };

        let client = reqwest::Client::new();
        let response = client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| Error::pairing(format!("failed to connect to server: {e}")))?;

        if response.status() == reqwest::StatusCode::UNAUTHORIZED {
            return Err(Error::pairing("invalid password"));
        }

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(Error::pairing(format!(
                "server returned {status}: {text}"
            )));
        }

        let pair_response: PairResponseBody = response
            .json()
            .await
            .map_err(|e| Error::pairing(format!("invalid response from server: {e}")))?;

        // Add the server itself as a device
        let server_node_id: iroh::NodeId = pair_response
            .server_node_id
            .parse()
            .map_err(|e| Error::pairing(format!("invalid server node_id: {e}")))?;
        self.registry
            .add(crate::registry::DeviceInfo::new(server_node_id, "Server"))
            .map_err(|e| Error::pairing(format!("failed to add server to registry: {e}")))?;

        // Add all returned devices to the local registry
        for device in &pair_response.devices {
            let device_node_id: iroh::NodeId = match device.node_id.parse() {
                Ok(id) => id,
                Err(e) => {
                    warn!(node_id = %device.node_id, error = %e, "Skipping device with invalid node_id");
                    continue;
                }
            };
            // Skip if already added (e.g., the server itself)
            if !self.registry.contains(&device_node_id) {
                if let Err(e) = self.registry.add(crate::registry::DeviceInfo::new(
                    device_node_id,
                    &device.name,
                )) {
                    warn!(node_id = %device.node_id, error = %e, "Failed to add device to registry");
                }
            }
        }

        // Persist registry to disk so it survives restarts
        self.registry
            .save()
            .map_err(|e| Error::pairing(format!("failed to save device registry: {e}")))?;

        info!(
            server_node_id = %pair_response.server_node_id,
            device_count = pair_response.devices.len(),
            "Pairing successful"
        );

        Ok(())
    }

    /// Open or create a document
    ///
    /// Documents are identified by path-like names (e.g., "notes/shopping-list").
    pub async fn open_doc(&self, name: &str) -> Result<Document> {
        debug!(name, "Opening document");
        self.store.open_doc(name).await
    }

    /// List all documents in a namespace
    ///
    /// Returns document names that start with the given prefix.
    pub fn list_docs(&self, prefix: &str) -> Result<Vec<String>> {
        self.store.list_docs(prefix)
    }

    /// Rename a document (move its file and update the index).
    pub async fn rename_doc(&self, old_name: &str, new_name: &str) -> Result<()> {
        self.store.rename_doc(old_name, new_name).await
    }

    /// Store a file as a blob
    ///
    /// Returns the content hash that can be used to reference the blob.
    /// Uses iroh-blobs for content-addressed storage with BLAKE3 hashing.
    pub async fn store_blob(&self, path: &Path) -> Result<String> {
        debug!(path = %path.display(), "Storing blob");
        self.store.store_blob(path).await
    }

    /// Store bytes as a blob
    ///
    /// Returns the content hash that can be used to reference the blob.
    /// Useful for storing data that's already in memory.
    pub async fn store_blob_bytes(&self, data: impl Into<bytes::Bytes>) -> Result<String> {
        debug!("Storing blob from bytes");
        self.store.store_blob_bytes(data).await
    }

    /// Retrieve a blob by its hash
    ///
    /// If the blob is not available locally, it will be fetched from peers.
    /// Returns the path to the local copy of the blob.
    pub async fn get_blob(&self, hash: &str) -> Result<std::path::PathBuf> {
        debug!(hash, "Retrieving blob");
        self.store.get_blob(hash).await
    }

    /// Get blob content as bytes
    ///
    /// Use with caution for large blobs as this loads everything into memory.
    pub async fn get_blob_bytes(&self, hash: &str) -> Result<bytes::Bytes> {
        debug!(hash, "Retrieving blob as bytes");
        self.store.get_blob_bytes(hash).await
    }

    /// Check if a blob exists locally
    pub async fn has_blob(&self, hash: &str) -> Result<bool> {
        self.store.has_blob(hash).await
    }

    /// Get the size of a blob in bytes
    pub async fn blob_size(&self, hash: &str) -> Result<u64> {
        self.store.blob_size(hash).await
    }

    /// Get the current sync status
    pub fn sync_status(&self) -> SyncStatus {
        self.sync_status.read().clone()
    }

    /// Check if the node is running
    pub fn is_running(&self) -> bool {
        matches!(&*self.state.read(), NodeState::Running { .. })
    }

    /// Shut down the node gracefully
    pub async fn shutdown(&self) -> Result<()> {
        info!("Shutting down HumanSync node");

        let (endpoint, sync_loop_handle, shutdown_tx) = {
            let mut state = self.state.write();
            match std::mem::replace(&mut *state, NodeState::Shutdown) {
                NodeState::Running { endpoint, sync_loop_handle, shutdown_tx, .. } => {
                    (endpoint, sync_loop_handle, shutdown_tx)
                }
                NodeState::Shutdown => {
                    debug!("Node already shut down");
                    return Ok(());
                }
            }
        };

        // Signal the sync loop to stop
        if shutdown_tx.send(true).is_err() {
            debug!("Sync loop already stopped (receiver dropped)");
        }

        // Wait for the sync loop to finish
        if let Some(handle) = sync_loop_handle {
            debug!("Waiting for sync loop to stop");
            // Give the sync loop a chance to stop gracefully
            match tokio::time::timeout(Duration::from_secs(5), handle).await {
                Ok(Ok(())) => debug!("Sync loop stopped gracefully"),
                Ok(Err(e)) => warn!("Sync loop task panicked: {e}"),
                Err(_) => {
                    warn!("Sync loop did not stop within timeout, aborting");
                }
            }
        }

        // Close the Iroh endpoint
        endpoint.close().await;

        info!("HumanSync node shut down successfully");
        Ok(())
    }

    /// Get a reference to the device registry
    pub fn registry(&self) -> &DeviceRegistryStore {
        &self.registry
    }

    /// Get a reference to the configuration
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Get the Iroh endpoint (if running)
    ///
    /// Returns None if the node has been shut down.
    pub fn endpoint(&self) -> Option<Endpoint> {
        let state = self.state.read();
        match &*state {
            NodeState::Running { endpoint, .. } => Some(endpoint.clone()),
            NodeState::Shutdown => None,
        }
    }

    /// Trigger an immediate sync cycle
    ///
    /// Normally sync runs automatically on the configured interval.
    /// This method can be called to trigger an immediate sync.
    pub async fn sync_now(&self) -> Result<()> {
        let endpoint = self.endpoint().ok_or(Error::Shutdown)?;

        // Manual sync uses a temporary failure counter (doesn't accumulate across calls)
        let mut failure_counts = HashMap::new();
        Self::run_sync_cycle(
            &endpoint,
            &self.store,
            &self.registry,
            &self.sync_status,
            &mut failure_counts,
        )
        .await;

        Ok(())
    }

    /// Enter pairing mode so that an unknown peer can connect and pair.
    ///
    /// The returned `PairingInfo` contains this device's node ID and name for
    /// generating a QR code or sharable link. The returned `Receiver` will
    /// resolve once a remote device successfully completes the handshake.
    pub fn enter_pairing_mode(
        &self,
        pin: &str,
    ) -> Result<(PairingInfo, oneshot::Receiver<PairingResult>)> {
        let node_id = self.node_id()?;
        let pin_hash: [u8; 32] = Sha256::digest(pin.as_bytes()).into();

        *self.pairing_pin_hash.write() = Some(pin_hash);

        let (tx, rx) = oneshot::channel();
        *self.pairing_result_tx.write() = Some(tx);

        self.pairing_mode.store(true, Ordering::SeqCst);

        info!("Entered pairing mode");

        Ok((
            PairingInfo {
                node_id: node_id.to_string(),
                device_name: self.config.device_name.clone(),
            },
            rx,
        ))
    }

    /// Cancel pairing mode without completing a pairing.
    pub fn cancel_pairing_mode(&self) {
        self.pairing_mode.store(false, Ordering::SeqCst);
        *self.pairing_pin_hash.write() = None;
        // Drop the sender so the receiver gets a RecvError
        *self.pairing_result_tx.write() = None;
        info!("Cancelled pairing mode");
    }

    /// Initiate pairing with a remote device by its node ID.
    ///
    /// This is the "scanner" side of the pairing flow. It connects to the
    /// remote device, sends the PIN hash and this device's name, and waits
    /// for acceptance.
    pub async fn pair_with_node_id(
        &self,
        remote_node_id: &NodeId,
        remote_name_hint: &str,
        pin: &str,
    ) -> Result<PairingResult> {
        let endpoint = self.endpoint().ok_or(Error::Shutdown)?;
        let pin_hash: [u8; 32] = Sha256::digest(pin.as_bytes()).into();
        let my_name = self.config.device_name.as_bytes();

        // NOTE: Do NOT add the remote to the registry before connecting.
        // If we did, the listener's background sync might connect to us,
        // and our accept loop would route it to handle_incoming_connection
        // (since we'd be in the listener's registry). Instead, we add the
        // remote only after the handshake succeeds.

        // Connect to remote with timeout
        let conn = tokio::time::timeout(
            Duration::from_secs(10),
            endpoint.connect(NodeAddr::new(*remote_node_id), ALPN),
        )
        .await
        .map_err(|_| Error::pairing("connection to remote timed out"))?
        .map_err(|e| Error::pairing(format!("failed to connect to remote: {e}")))?;

        // Open a bidi stream
        let (mut send, mut recv) = conn
            .open_bi()
            .await
            .map_err(|e| Error::pairing(format!("failed to open stream: {e}")))?;

        // Send: [0xFE][pin_hash: 32][name_len: u16 BE][name_bytes]
        let name_len = (my_name.len() as u16).to_be_bytes();
        let mut payload = Vec::with_capacity(1 + 32 + 2 + my_name.len());
        payload.push(PAIR_REQUEST_TAG);
        payload.extend_from_slice(&pin_hash);
        payload.extend_from_slice(&name_len);
        payload.extend_from_slice(my_name);

        send.write_all(&payload)
            .await
            .map_err(|e| Error::pairing(format!("failed to send pairing request: {e}")))?;
        // Don't finish() yet — acceptor waits for our FIN as confirmation we read the response

        // Read response: first byte is accept/reject
        let mut response_tag = [0u8; 1];
        recv.read_exact(&mut response_tag)
            .await
            .map_err(|e| Error::pairing(format!("failed to read pairing response: {e}")))?;

        if response_tag[0] == 0x00 {
            send.finish().ok();
            return Err(Error::pairing("PIN rejected by remote device"));
        }

        // Accepted: read [name_len: u16 BE][name_bytes]
        let mut peer_name_len_buf = [0u8; 2];
        recv.read_exact(&mut peer_name_len_buf)
            .await
            .map_err(|e| Error::pairing(format!("failed to read peer name length: {e}")))?;
        let peer_name_len = u16::from_be_bytes(peer_name_len_buf) as usize;
        if peer_name_len == 0 || peer_name_len > 256 {
            return Err(Error::pairing(format!("invalid peer name length: {peer_name_len}")));
        }

        let mut peer_name_buf = vec![0u8; peer_name_len];
        recv.read_exact(&mut peer_name_buf)
            .await
            .map_err(|e| Error::pairing(format!("failed to read peer name: {e}")))?;

        let peer_name = String::from_utf8(peer_name_buf)
            .unwrap_or_else(|_| remote_name_hint.to_string());

        // Now finish our send stream — signals to acceptor that we received the response
        send.finish()
            .map_err(|e| Error::pairing(format!("failed to finish send: {e}")))?;

        // Now add the remote to our registry (only after successful handshake)
        self.registry
            .add(crate::registry::DeviceInfo::new(*remote_node_id, &peer_name))
            .map_err(|e| Error::pairing(format!("failed to add peer to registry: {e}")))?;
        self.registry
            .save()
            .map_err(|e| Error::pairing(format!("failed to save registry: {e}")))?;

        // Trigger immediate sync
        let _ = self.sync_now().await;

        info!(remote = %remote_node_id, name = %peer_name, "Pairing completed (scanner side)");

        Ok(PairingResult {
            node_id: remote_node_id.to_string(),
            device_name: peer_name,
        })
    }

    /// Handle the pairing handshake on the accepting side.
    ///
    /// Reads the pairing request from the incoming connection, verifies the PIN,
    /// and if accepted adds the remote to the registry.
    async fn handle_pairing_handshake(
        conn: iroh::endpoint::Connection,
        remote_node_id: NodeId,
        registry: &Arc<DeviceRegistryStore>,
        pairing_pin_hash: &Arc<RwLock<Option<[u8; 32]>>>,
        device_name: &str,
    ) -> Result<PairingResult> {
        let (mut send, mut recv) = conn
            .accept_bi()
            .await
            .map_err(|e| Error::pairing(format!("failed to accept stream: {e}")))?;

        // Read tag byte
        let mut tag = [0u8; 1];
        recv.read_exact(&mut tag)
            .await
            .map_err(|e| Error::pairing(format!("failed to read tag: {e}")))?;

        if tag[0] != PAIR_REQUEST_TAG {
            return Err(Error::pairing(format!(
                "unexpected tag byte: {:#x}",
                tag[0]
            )));
        }

        // Read 32 bytes of PIN hash
        let mut received_hash = [0u8; 32];
        recv.read_exact(&mut received_hash)
            .await
            .map_err(|e| Error::pairing(format!("failed to read PIN hash: {e}")))?;

        // Verify PIN hash
        let expected_hash = pairing_pin_hash.read().ok_or_else(|| {
            Error::pairing("no PIN hash set (pairing mode was cancelled)")
        })?;

        if received_hash != expected_hash {
            // Reject: send [0x00]
            let _ = send.write_all(&[0x00]).await;
            let _ = send.finish();
            return Err(Error::pairing("PIN mismatch"));
        }

        // Read name_len (u16 BE) and name_bytes
        let mut name_len_buf = [0u8; 2];
        recv.read_exact(&mut name_len_buf)
            .await
            .map_err(|e| Error::pairing(format!("failed to read name length: {e}")))?;
        let name_len = u16::from_be_bytes(name_len_buf) as usize;
        if name_len == 0 || name_len > 256 {
            return Err(Error::pairing(format!("invalid device name length: {name_len}")));
        }

        let mut name_buf = vec![0u8; name_len];
        recv.read_exact(&mut name_buf)
            .await
            .map_err(|e| Error::pairing(format!("failed to read name: {e}")))?;

        let remote_name =
            String::from_utf8(name_buf).unwrap_or_else(|_| "Device".to_string());

        // Accept: add remote to registry
        registry
            .add(crate::registry::DeviceInfo::new(remote_node_id, &remote_name))
            .map_err(|e| Error::pairing(format!("failed to add peer to registry: {e}")))?;
        registry
            .save()
            .map_err(|e| Error::pairing(format!("failed to save registry: {e}")))?;

        // Send acceptance: [0x01][name_len: u16 BE][name_bytes]
        let our_name = device_name.as_bytes();
        let our_name_len = (our_name.len() as u16).to_be_bytes();
        let mut response = Vec::with_capacity(1 + 2 + our_name.len());
        response.push(0x01);
        response.extend_from_slice(&our_name_len);
        response.extend_from_slice(our_name);

        send.write_all(&response)
            .await
            .map_err(|e| Error::pairing(format!("failed to send acceptance: {e}")))?;
        send.finish()
            .map_err(|e| Error::pairing(format!("failed to finish stream: {e}")))?;

        // Wait for scanner to close its send stream (FIN), confirming it received our response.
        // Without this, returning drops the Connection which sends CONNECTION_CLOSE before
        // our response data reaches the scanner, causing "connection lost" on their side.
        let _ = tokio::time::timeout(
            Duration::from_secs(5),
            recv.read_to_end(256),
        )
        .await;

        info!(remote = %remote_node_id, name = %remote_name, "Pairing completed (acceptor side)");

        Ok(PairingResult {
            node_id: remote_node_id.to_string(),
            device_name: remote_name,
        })
    }

    /// Start an accept loop for incoming P2P connections.
    ///
    /// This allows other devices to connect to us directly (e.g., via mDNS on
    /// the same LAN). Incoming connections are gated against the device registry,
    /// with a special branch for pairing mode.
    fn start_accept_loop(
        endpoint: Endpoint,
        store: Arc<Store>,
        registry: Arc<DeviceRegistryStore>,
        pairing_mode: Arc<AtomicBool>,
        pairing_result_tx: Arc<RwLock<Option<oneshot::Sender<PairingResult>>>>,
        pairing_pin_hash: Arc<RwLock<Option<[u8; 32]>>>,
        device_name: String,
        sync_status: Arc<RwLock<SyncStatus>>,
    ) {
        tokio::spawn(async move {
            info!("Client accept loop started");
            loop {
                let incoming = match endpoint.accept().await {
                    Some(incoming) => incoming,
                    None => {
                        debug!("Endpoint closed, stopping accept loop");
                        break;
                    }
                };

                let conn = match incoming.await {
                    Ok(conn) => conn,
                    Err(e) => {
                        debug!(error = %e, "Failed to accept incoming connection");
                        continue;
                    }
                };

                let remote_node_id = match conn.remote_node_id() {
                    Ok(id) => id,
                    Err(e) => {
                        debug!(error = %e, "Incoming connection has no remote NodeId");
                        continue;
                    }
                };

                // Gate against device registry
                if registry.is_authorized(&remote_node_id) {
                    info!(remote = %remote_node_id, "Accepted P2P connection");

                    let store = store.clone();
                    tokio::spawn(async move {
                        if let Err(e) = Self::handle_incoming_connection(conn, store).await {
                            debug!(remote = %remote_node_id, error = %e, "Incoming connection handler finished");
                        }
                    });
                } else if pairing_mode.load(Ordering::SeqCst) {
                    // Unknown peer while in pairing mode -- handle pairing handshake.
                    // We keep pairing_mode=true until the handshake succeeds so that
                    // a failed attempt (wrong PIN, timeout, etc.) doesn't lock out retries.
                    info!(remote = %remote_node_id, "Accepting unknown connection for pairing");
                    let registry = registry.clone();
                    let pairing_result_tx = pairing_result_tx.clone();
                    let pairing_pin_hash = pairing_pin_hash.clone();
                    let pairing_mode = pairing_mode.clone();
                    let device_name = device_name.clone();
                    let endpoint = endpoint.clone();
                    let store = store.clone();
                    let sync_status = sync_status.clone();
                    tokio::spawn(async move {
                        match Self::handle_pairing_handshake(
                            conn,
                            remote_node_id,
                            &registry,
                            &pairing_pin_hash,
                            &device_name,
                        )
                        .await
                        {
                            Ok(result) => {
                                // Handshake succeeded — now disable pairing mode
                                pairing_mode.store(false, Ordering::SeqCst);

                                // Send the result (scope the guard so it's dropped before the await)
                                {
                                    let mut guard = pairing_result_tx.write();
                                    if let Some(tx) = guard.take() {
                                        let _ = tx.send(result);
                                    }
                                }

                                // Give the scanner time to finish pairing and add us to their registry
                                tokio::time::sleep(Duration::from_secs(2)).await;

                                // Trigger sync so the new peer's docs are exchanged
                                let mut failure_counts = HashMap::new();
                                Self::run_sync_cycle(
                                    &endpoint,
                                    &store,
                                    &registry,
                                    &sync_status,
                                    &mut failure_counts,
                                )
                                .await;
                                info!("Post-pairing sync completed (acceptor side)");
                            }
                            Err(e) => {
                                // Pairing failed — pairing_mode stays true so user can retry
                                warn!(error = %e, "Pairing handshake failed");
                            }
                        }
                    });
                } else {
                    debug!(remote = %remote_node_id, "Rejected connection from unknown device");
                }
            }
            info!("Client accept loop stopped");
        });
    }

    /// Handle an accepted incoming connection by routing streams to protocol handlers.
    async fn handle_incoming_connection(
        conn: iroh::endpoint::Connection,
        store: Arc<Store>,
    ) -> Result<()> {
        loop {
            let (send, mut recv) = match conn.accept_bi().await {
                Ok(streams) => streams,
                Err(_) => break, // Connection closed — normal
            };

            let mut tag = [0u8; 1];
            if recv.read_exact(&mut tag).await.is_err() {
                continue;
            }

            let store = store.clone();

            match tag[0] {
                crate::sync::BLOB_REQUEST_MSG => {
                    tokio::spawn(async move {
                        if let Some(blob_store) = store.blob_store() {
                            let mut send = send;
                            let mut recv = recv;
                            let _ = handle_blob_request(&blob_store, &mut send, &mut recv).await;
                        }
                    });
                }
                crate::sync::DOC_LIST_REQUEST => {
                    tokio::spawn(async move {
                        if let Ok(doc_names) = store.list_docs("") {
                            let _ = handle_doc_list_request(send, &doc_names).await;
                        }
                    });
                }
                first_byte => {
                    // Doc sync — first byte is part of the doc name length prefix
                    tokio::spawn(async move {
                        if let Err(e) = Self::handle_incoming_doc_sync(store, send, recv, first_byte).await {
                            debug!(error = %e, "Incoming doc sync finished");
                        }
                    });
                }
            }
        }
        Ok(())
    }

    /// Handle an incoming doc sync stream (same protocol as server-side `handle_doc_sync`).
    async fn handle_incoming_doc_sync(
        store: Arc<Store>,
        send: iroh::endpoint::SendStream,
        mut recv: iroh::endpoint::RecvStream,
        first_byte: u8,
    ) -> Result<()> {
        // Read remaining 3 bytes of the 4-byte doc name length
        let mut remaining_len = [0u8; 3];
        recv.read_exact(&mut remaining_len)
            .await
            .map_err(|e| Error::sync(format!("failed to read doc name length: {e}")))?;

        let name_len = u32::from_be_bytes([
            first_byte,
            remaining_len[0],
            remaining_len[1],
            remaining_len[2],
        ]) as usize;

        if name_len == 0 || name_len > 4096 {
            return Err(Error::sync(format!("invalid doc name length: {name_len}")));
        }

        // Read the doc name
        let mut name_buf = vec![0u8; name_len];
        recv.read_exact(&mut name_buf)
            .await
            .map_err(|e| Error::sync(format!("failed to read doc name: {e}")))?;

        let doc_name = String::from_utf8(name_buf)
            .map_err(|e| Error::sync(format!("invalid doc name: {e}")))?;

        debug!(doc = %doc_name, "Incoming doc sync");

        // Open or create the local document
        let local_doc = store.open_doc(&doc_name).await?;

        // Run the sync protocol
        run_sync_protocol(send, recv, &local_doc).await?;

        // Save the synced document
        store
            .save_doc_with_index(&local_doc)
            .await
            .map_err(|e| Error::sync(format!("failed to save synced document: {e}")))?;

        info!(doc = %doc_name, "Incoming doc sync completed");
        Ok(())
    }

    /// Start the background sync loop
    ///
    /// Returns a JoinHandle that can be used to wait for the task to complete.
    fn start_sync_loop(
        endpoint: Endpoint,
        store: Arc<Store>,
        registry: Arc<DeviceRegistryStore>,
        sync_status: Arc<RwLock<SyncStatus>>,
        sync_interval_secs: u64,
        mut shutdown_rx: watch::Receiver<bool>,
    ) -> JoinHandle<()> {
        let sync_interval = Duration::from_secs(sync_interval_secs);

        tokio::spawn(async move {
            info!(
                interval_secs = sync_interval_secs,
                "Background sync loop started"
            );

            let mut failure_counts: HashMap<NodeId, u32> = HashMap::new();

            loop {
                // Use tokio::select! for cancellation
                tokio::select! {
                    // Check for shutdown signal
                    _ = shutdown_rx.changed() => {
                        if *shutdown_rx.borrow() {
                            info!("Sync loop received shutdown signal");
                            break;
                        }
                    }
                    // Sleep for the sync interval
                    _ = tokio::time::sleep(sync_interval) => {
                        // Run a sync cycle
                        Self::run_sync_cycle(&endpoint, &store, &registry, &sync_status, &mut failure_counts).await;
                    }
                }
            }

            info!("Background sync loop stopped");
        })
    }

    /// Migrate vault documents after syncing `_vaults.automerge`.
    ///
    /// Reads the shared vault registry doc, compares vault IDs by name,
    /// picks the canonical ID (earliest `created_at`), and renames document
    /// prefixes from the losing ID to the winning ID.
    ///
    /// Returns the number of documents migrated.
    pub async fn migrate_vault_docs(&self) -> Result<usize> {
        let vaults_doc_name = "_vaults.automerge";
        let doc = match self.store.open_doc(vaults_doc_name).await {
            Ok(d) => d,
            Err(_) => return Ok(0), // No shared vault doc yet
        };

        let keys = doc.keys()?;
        if keys.is_empty() {
            return Ok(0);
        }

        // Parse shared vault entries: vault_id -> (name, created_at)
        #[derive(serde::Deserialize)]
        struct SharedVault {
            name: String,
            created_at: String,
        }

        let mut shared_vaults: Vec<(String, String, String)> = Vec::new(); // (id, name, created_at)
        for key in &keys {
            if key.starts_with('_') {
                continue;
            }
            // Read as SharedVault directly — doc.get::<String>() fails because
            // automerge_value_to_json auto-parses JSON strings into objects.
            if let Ok(Some(sv)) = doc.get::<SharedVault>(key) {
                shared_vaults.push((key.clone(), sv.name, sv.created_at));
            }
        }

        if shared_vaults.is_empty() {
            return Ok(0);
        }

        // Group vaults by name to find conflicts
        let mut by_name: HashMap<String, Vec<(String, String)>> = HashMap::new(); // name -> [(id, created_at)]
        for (id, name, created_at) in &shared_vaults {
            by_name
                .entry(name.clone())
                .or_default()
                .push((id.clone(), created_at.clone()));
        }

        let mut total_migrated = 0;

        for (_name, mut entries) in by_name {
            if entries.len() < 2 {
                continue;
            }

            // Sort by created_at -- earliest wins
            entries.sort_by(|a, b| a.1.cmp(&b.1));
            let canonical_id = &entries[0].0;

            // Migrate docs from all non-canonical IDs to the canonical one
            for (loser_id, _) in &entries[1..] {
                if loser_id == canonical_id {
                    continue;
                }
                let old_prefix = format!("humandocs/vault-{loser_id}/doc-");
                if let Ok(old_docs) = self.store.list_docs(&old_prefix) {
                    let new_prefix = format!("humandocs/vault-{canonical_id}/doc-");
                    for old_name in old_docs {
                        let new_name = old_name.replace(&old_prefix, &new_prefix);
                        info!(
                            old_name = %old_name,
                            new_name = %new_name,
                            "migrate_vault_docs: renaming document to canonical vault prefix"
                        );
                        if let Err(e) = self.store.rename_doc(&old_name, &new_name).await {
                            warn!(error = %e, old_name = %old_name, "Failed to migrate document");
                        } else {
                            total_migrated += 1;
                        }
                    }
                }
            }
        }

        if total_migrated > 0 {
            info!(total_migrated, "Vault document migration completed");
        }

        Ok(total_migrated)
    }

    /// Run a single sync cycle
    ///
    /// This syncs with all known peers in the device registry.
    /// `failure_counts` tracks consecutive connection failures per peer across cycles.
    /// After `MAX_CONSECUTIVE_FAILURES` consecutive failures, a peer is automatically
    /// removed from the registry (unless it is the server).
    async fn run_sync_cycle(
        endpoint: &Endpoint,
        store: &Arc<Store>,
        registry: &Arc<DeviceRegistryStore>,
        sync_status: &Arc<RwLock<SyncStatus>>,
        failure_counts: &mut HashMap<NodeId, u32>,
    ) {
        debug!("Starting sync cycle");

        let devices = registry.list();
        if devices.is_empty() {
            debug!("No devices in registry, skipping sync");
            return;
        }

        let my_node_id = endpoint.node_id();
        let mut peers_connected = 0;
        let mut docs_synced = 0;
        let mut total_docs = 0usize;

        for device in &devices {
            // Skip ourselves
            if device.node_id == my_node_id {
                continue;
            }

            // Check if the peer is still in the registry (could have been removed)
            if !registry.contains(&device.node_id) {
                debug!(peer = %device.node_id, "Peer no longer in registry, skipping");
                continue;
            }

            // Try to sync with this peer
            match Self::sync_with_peer(endpoint, store, &device.node_id).await {
                Ok((synced_count, doc_count)) => {
                    peers_connected += 1;
                    docs_synced += synced_count;
                    total_docs = total_docs.max(doc_count);
                    // Mark peer as online (connected + synced)
                    let _ = registry.touch(&device.node_id);
                    // Reset failure counter on success
                    failure_counts.remove(&device.node_id);
                    debug!(
                        peer = %device.node_id,
                        docs_synced = synced_count,
                        "Sync with peer completed"
                    );
                }
                Err(e) => {
                    let count = failure_counts.entry(device.node_id).or_insert(0);
                    *count += 1;
                    warn!(
                        peer = %device.node_id,
                        consecutive_failures = *count,
                        error = %e,
                        "Failed to sync with peer"
                    );

                    // Log a warning after many consecutive failures but never
                    // auto-remove peers — that would permanently break pairing.
                    // Users can manually remove devices from the UI.
                    if *count == MAX_CONSECUTIVE_FAILURES {
                        warn!(
                            peer = %device.node_id,
                            name = %device.name,
                            failures = *count,
                            "Peer unreachable for extended period"
                        );
                    }
                }
            }
        }

        // Update sync status
        {
            let mut status = sync_status.write();
            status.peers_connected = peers_connected;
            if peers_connected > 0 {
                // Only mark docs_pending = 0 when all docs synced successfully
                if total_docs > 0 && docs_synced >= total_docs {
                    status.docs_pending = 0;
                } else if total_docs > docs_synced {
                    status.docs_pending = total_docs - docs_synced;
                }
                status.last_sync = Some(Utc::now());
            }
        }

        // After syncing, reload the device registry from the _devices doc on disk.
        // This picks up newly paired devices that were synced from the server.
        if let Err(e) = registry.reload_from_disk() {
            warn!(error = %e, "Failed to reload device registry after sync");
        }

        // Run vault document migration after sync to move docs under canonical vault IDs.
        // This is a no-op if _vaults.automerge doesn't exist or has no conflicts.
        {
            // Build a temporary HumanSync-like context to call migrate_vault_docs.
            // We only need store access, which we already have.
            let vaults_doc_name = "_vaults.automerge";
            if let Ok(doc) = store.open_doc(vaults_doc_name).await {
                if let Ok(keys) = doc.keys() {
                    if !keys.is_empty() {
                        #[derive(serde::Deserialize)]
                        struct SharedVault {
                            name: String,
                            created_at: String,
                        }

                        let mut shared_vaults: Vec<(String, String, String)> = Vec::new();
                        for key in &keys {
                            if key.starts_with('_') {
                                continue;
                            }
                            // Read as SharedVault directly — doc.get::<String>() fails
                            // because automerge_value_to_json auto-parses JSON strings
                            // into objects, so we must deserialize as the target struct.
                            if let Ok(Some(sv)) = doc.get::<SharedVault>(key) {
                                shared_vaults.push((key.clone(), sv.name, sv.created_at));
                            }
                        }

                        // Group by name and migrate
                        let mut by_name: HashMap<String, Vec<(String, String)>> = HashMap::new();
                        for (id, name, created_at) in &shared_vaults {
                            by_name
                                .entry(name.clone())
                                .or_default()
                                .push((id.clone(), created_at.clone()));
                        }

                        for (_name, mut entries) in by_name {
                            if entries.len() < 2 {
                                continue;
                            }
                            entries.sort_by(|a, b| a.1.cmp(&b.1));
                            let canonical_id = entries[0].0.clone();
                            for (loser_id, _) in &entries[1..] {
                                if *loser_id == canonical_id {
                                    continue;
                                }
                                let old_prefix = format!("humandocs/vault-{loser_id}/doc-");
                                if let Ok(old_docs) = store.list_docs(&old_prefix) {
                                    let new_prefix =
                                        format!("humandocs/vault-{canonical_id}/doc-");
                                    for old_name in old_docs {
                                        let new_name =
                                            old_name.replace(&old_prefix, &new_prefix);
                                        info!(
                                            old_name = %old_name,
                                            new_name = %new_name,
                                            "sync loop: migrating document to canonical vault prefix"
                                        );
                                        if let Err(e) =
                                            store.rename_doc(&old_name, &new_name).await
                                        {
                                            warn!(
                                                error = %e,
                                                old_name = %old_name,
                                                "Failed to migrate document in sync loop"
                                            );
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        debug!(
            peers_connected,
            docs_synced,
            "Sync cycle completed"
        );
    }

    /// Sync documents with a specific peer
    ///
    /// Returns `(synced_count, total_doc_count)` -- the number of documents
    /// successfully synced and the total number of documents attempted.
    ///
    /// The sync process:
    /// 1. Request the peer's document list (doc discovery) with one retry
    /// 2. Compute the union of local and remote doc names
    /// 3. Sort so system docs (`_`-prefixed) sync first
    /// 4. Sync each document in the union
    /// 5. Sync blobs referenced by all documents
    async fn sync_with_peer(
        endpoint: &Endpoint,
        store: &Arc<Store>,
        peer_node_id: &NodeId,
    ) -> Result<(usize, usize)> {
        debug!(peer = %peer_node_id, "Connecting to peer for sync");

        let node_addr = NodeAddr::new(*peer_node_id);

        // Connect to the peer with a short timeout to avoid blocking on dead peers
        let conn = tokio::time::timeout(
            Duration::from_secs(3),
            endpoint.connect(node_addr, ALPN),
        )
        .await
        .map_err(|_| Error::sync("failed to connect to peer: timed out"))?
        .map_err(|e| Error::sync(format!("failed to connect to peer: {e}")))?;

        debug!(peer = %peer_node_id, "Connected to peer");

        // Step 1: Request remote doc list for discovery (with one retry)
        let mut doc_list_failed = false;
        let remote_docs = match request_doc_list(&conn).await {
            Ok(docs) => {
                debug!(peer = %peer_node_id, count = docs.len(), "Received remote doc list");
                docs
            }
            Err(first_err) => {
                warn!(peer = %peer_node_id, error = %first_err, "Failed to get remote doc list, retrying in 1s");
                tokio::time::sleep(Duration::from_secs(1)).await;
                match request_doc_list(&conn).await {
                    Ok(docs) => {
                        debug!(peer = %peer_node_id, count = docs.len(), "Received remote doc list on retry");
                        docs
                    }
                    Err(retry_err) => {
                        warn!(peer = %peer_node_id, error = %retry_err, "Doc list retry also failed, syncing local only");
                        doc_list_failed = true;
                        Vec::new()
                    }
                }
            }
        };

        // Step 2: Compute union of local and remote doc names
        let local_docs = store.list_docs("")?;
        let mut all_doc_names = local_docs.clone();
        for name in &remote_docs {
            if !all_doc_names.contains(name) {
                debug!(doc = %name, "Discovered new doc from peer");
                all_doc_names.push(name.clone());
            }
        }

        // Step 3: Sort so system docs (_-prefixed) sync first
        all_doc_names.sort_by(|a, b| {
            let a_system = a.starts_with('_');
            let b_system = b.starts_with('_');
            match (a_system, b_system) {
                (true, false) => std::cmp::Ordering::Less,
                (false, true) => std::cmp::Ordering::Greater,
                _ => a.cmp(b),
            }
        });

        // Step 4: Sync each document
        let total_doc_count = all_doc_names.len();
        let mut synced_count = 0;

        for doc_name in &all_doc_names {
            // Open (or create) the local document
            let local_doc = match store.open_doc(doc_name).await {
                Ok(doc) => doc,
                Err(e) => {
                    warn!(doc = %doc_name, error = %e, "Failed to open local document");
                    continue;
                }
            };

            // Sync this document with the peer
            match sync_docs(&conn, &local_doc).await {
                Ok(()) => {
                    // Save the synced document
                    if let Err(e) = local_doc.save() {
                        error!(doc = %doc_name, error = %e, "Failed to save synced document");
                    } else {
                        synced_count += 1;
                    }
                }
                Err(e) => {
                    warn!(doc = %doc_name, error = %e, "Failed to sync document");
                }
            }
        }

        // Step 5: Sync blobs referenced by all documents (including newly discovered ones)
        let mut all_blob_hashes = Vec::new();
        for doc_name in &all_doc_names {
            if let Ok(doc) = store.open_doc(doc_name).await {
                let doc_bytes = doc.save_bytes();
                let refs = crate::sync::blobs::extract_blob_refs(&doc_bytes);
                all_blob_hashes.extend(refs);
            }
        }

        // Deduplicate
        all_blob_hashes.sort();
        all_blob_hashes.dedup();

        if !all_blob_hashes.is_empty() {
            if let Some(blob_store) = store.blob_store() {
                let peer_node_addr = NodeAddr::new(*peer_node_id);
                match crate::sync::blobs::sync_blobs_with_peer(
                    &blob_store,
                    endpoint,
                    &peer_node_addr,
                    &all_blob_hashes,
                )
                .await
                {
                    Ok(count) => {
                        debug!(count, "Blobs synced from peer");
                    }
                    Err(e) => {
                        warn!(error = %e, "Failed to sync blobs from peer");
                    }
                }
            }
        }

        // If doc list discovery failed, don't report full sync success even
        // if all local docs synced -- we may have missed remote-only docs.
        if doc_list_failed {
            Ok((synced_count, total_doc_count + 1))
        } else {
            Ok((synced_count, total_doc_count))
        }
    }
}

/// Request body for the pairing API
#[derive(serde::Serialize)]
struct PairRequestBody {
    node_id: String,
    device_name: String,
    password: String,
}

/// Response body from the pairing API
#[derive(serde::Deserialize)]
struct PairResponseBody {
    server_node_id: String,
    devices: Vec<PairDeviceInfo>,
}

/// Device info returned from the pairing API
#[derive(serde::Deserialize)]
struct PairDeviceInfo {
    node_id: String,
    name: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::DeviceInfo;
    use iroh::SecretKey;
    use tempfile::TempDir;

    #[tokio::test]
    #[ignore = "requires network connectivity for n0 discovery - run with --ignored"]
    async fn test_node_init_and_shutdown() {
        let temp_dir = TempDir::new().unwrap();
        let config = Config::new(temp_dir.path());

        let node = HumanSync::init(config).await.unwrap();
        assert!(node.is_running());

        let node_id = node.node_id().unwrap();
        assert!(!node_id.to_string().is_empty());

        node.shutdown().await.unwrap();
        assert!(!node.is_running());
    }

    #[tokio::test]
    #[ignore = "requires network connectivity for n0 discovery - run with --ignored"]
    async fn test_persistent_identity() {
        let temp_dir = TempDir::new().unwrap();
        let config = Config::new(temp_dir.path());

        // First init
        let node1 = HumanSync::init(config.clone()).await.unwrap();
        let node_id1 = node1.node_id().unwrap();
        node1.shutdown().await.unwrap();

        // Second init should have same identity
        let node2 = HumanSync::init(config).await.unwrap();
        let node_id2 = node2.node_id().unwrap();
        node2.shutdown().await.unwrap();

        assert_eq!(node_id1, node_id2);
    }

    #[tokio::test]
    #[ignore = "requires network connectivity for n0 discovery - run with --ignored"]
    async fn test_sync_loop_starts_and_stops() {
        let temp_dir = TempDir::new().unwrap();
        // Use a short sync interval for testing
        let config = Config::new(temp_dir.path()).with_sync_interval(1);

        let node = HumanSync::init(config).await.unwrap();
        assert!(node.is_running());

        // Check initial sync status
        let status = node.sync_status();
        assert_eq!(status.peers_connected, 0);
        assert_eq!(status.docs_pending, 0);
        assert!(status.last_sync.is_none()); // No sync yet

        // Shutdown should stop the sync loop gracefully
        node.shutdown().await.unwrap();
        assert!(!node.is_running());
    }

    #[tokio::test]
    #[ignore = "requires network connectivity for n0 discovery - run with --ignored"]
    async fn test_sync_status_updated_after_sync() {
        let temp_dir = TempDir::new().unwrap();
        let config = Config::new(temp_dir.path()).with_sync_interval(1);

        let node = HumanSync::init(config).await.unwrap();

        // Add a device to the registry (but not a real one, so sync will fail)
        // This tests that the sync loop runs and updates status
        let fake_node_id = SecretKey::generate(rand::thread_rng()).public();
        let device_info = DeviceInfo::new(fake_node_id, "Fake Device");
        let _ = node.registry().add(device_info);

        // Wait for a sync cycle to run
        tokio::time::sleep(Duration::from_secs(2)).await;

        // Check that last_sync is now set (even if sync failed)
        let status = node.sync_status();
        // The sync will have run, but may have 0 peers connected if connection failed
        // That's expected - we just want to verify the loop is running
        assert!(status.last_sync.is_some(), "Sync loop should have run");

        node.shutdown().await.unwrap();
    }

    #[tokio::test]
    #[ignore = "requires network connectivity for n0 discovery - run with --ignored"]
    async fn test_sync_now_manual_trigger() {
        let temp_dir = TempDir::new().unwrap();
        // Use a long sync interval to ensure manual trigger is separate
        let config = Config::new(temp_dir.path()).with_sync_interval(3600);

        let node = HumanSync::init(config).await.unwrap();

        // Verify initial state
        assert!(node.sync_status().last_sync.is_none());

        // Trigger manual sync (will complete quickly since no peers)
        node.sync_now().await.unwrap();

        // Check that last_sync is now set
        let status = node.sync_status();
        assert!(status.last_sync.is_some(), "Manual sync should have run");

        node.shutdown().await.unwrap();
    }

    #[tokio::test]
    #[ignore = "requires network connectivity for n0 discovery - run with --ignored"]
    async fn test_sync_skips_self() {
        let temp_dir = TempDir::new().unwrap();
        let config = Config::new(temp_dir.path()).with_sync_interval(3600);

        let node = HumanSync::init(config).await.unwrap();

        // Add ourselves to the registry (should be skipped during sync)
        let self_node_id = node.node_id().unwrap();
        let device_info = DeviceInfo::new(self_node_id, "Self");
        let _ = node.registry().add(device_info);

        // Trigger manual sync
        node.sync_now().await.unwrap();

        // Should complete without trying to connect to ourselves
        let status = node.sync_status();
        assert!(status.last_sync.is_some());
        assert_eq!(status.peers_connected, 0); // Didn't connect to anyone

        node.shutdown().await.unwrap();
    }

    #[tokio::test]
    #[ignore = "requires network connectivity for n0 discovery - run with --ignored"]
    async fn test_multiple_shutdown_calls() {
        let temp_dir = TempDir::new().unwrap();
        let config = Config::new(temp_dir.path());

        let node = HumanSync::init(config).await.unwrap();
        assert!(node.is_running());

        // First shutdown
        node.shutdown().await.unwrap();
        assert!(!node.is_running());

        // Second shutdown should be a no-op
        node.shutdown().await.unwrap();
        assert!(!node.is_running());
    }

    // Unit tests that don't require network

    #[test]
    fn test_sync_status_default() {
        let status = SyncStatus::default();
        assert_eq!(status.peers_connected, 0);
        assert_eq!(status.docs_pending, 0);
        assert!(status.last_sync.is_none());
    }

    #[test]
    fn test_sync_status_clone() {
        let mut status = SyncStatus::default();
        status.peers_connected = 5;
        status.docs_pending = 10;
        status.last_sync = Some(Utc::now());

        let cloned = status.clone();
        assert_eq!(cloned.peers_connected, 5);
        assert_eq!(cloned.docs_pending, 10);
        assert!(cloned.last_sync.is_some());
    }
}
