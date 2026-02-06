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

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use iroh::{Endpoint, NodeAddr, NodeId, SecretKey};
use parking_lot::RwLock;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tracing::{debug, error, info, warn};

use crate::config::Config;
use crate::doc::Document;
use crate::error::{Error, Result};
use crate::identity::load_or_create_identity;
use crate::registry::DeviceRegistry;
use crate::store::Store;
use crate::sync::automerge::{request_doc_list, sync_docs};
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
    /// Device registry
    registry: Arc<DeviceRegistry>,
    /// Sync status
    sync_status: Arc<RwLock<SyncStatus>>,
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

        // Initialize device registry
        let registry = Arc::new(DeviceRegistry::new());

        // Create shutdown signal channel
        let (shutdown_tx, shutdown_rx) = watch::channel(false);

        let sync_status = Arc::new(RwLock::new(SyncStatus::default()));

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
        };

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
        self.registry.add(crate::registry::DeviceInfo::new(
            server_node_id,
            "Server",
        ));

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
                self.registry.add(crate::registry::DeviceInfo::new(
                    device_node_id,
                    &device.name,
                ));
            }
        }

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
    pub fn registry(&self) -> &DeviceRegistry {
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

        Self::run_sync_cycle(
            &endpoint,
            &self.store,
            &self.registry,
            &self.sync_status,
        )
        .await;

        Ok(())
    }

    /// Start the background sync loop
    ///
    /// Returns a JoinHandle that can be used to wait for the task to complete.
    fn start_sync_loop(
        endpoint: Endpoint,
        store: Arc<Store>,
        registry: Arc<DeviceRegistry>,
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
                        Self::run_sync_cycle(&endpoint, &store, &registry, &sync_status).await;
                    }
                }
            }

            info!("Background sync loop stopped");
        })
    }

    /// Run a single sync cycle
    ///
    /// This syncs with all known peers in the device registry.
    async fn run_sync_cycle(
        endpoint: &Endpoint,
        store: &Arc<Store>,
        registry: &Arc<DeviceRegistry>,
        sync_status: &Arc<RwLock<SyncStatus>>,
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
                Ok(synced_count) => {
                    peers_connected += 1;
                    docs_synced += synced_count;
                    registry.touch(&device.node_id);
                    debug!(
                        peer = %device.node_id,
                        docs_synced = synced_count,
                        "Sync with peer completed"
                    );
                }
                Err(e) => {
                    warn!(
                        peer = %device.node_id,
                        error = %e,
                        "Failed to sync with peer"
                    );
                }
            }
        }

        // Update sync status
        {
            let mut status = sync_status.write();
            status.peers_connected = peers_connected;
            status.docs_pending = 0; // Reset after sync
            status.last_sync = Some(Utc::now());
        }

        debug!(
            peers_connected,
            docs_synced,
            "Sync cycle completed"
        );
    }

    /// Sync documents with a specific peer
    ///
    /// Returns the number of documents synced.
    ///
    /// The sync process:
    /// 1. Request the peer's document list (doc discovery)
    /// 2. Compute the union of local and remote doc names
    /// 3. Sync each document in the union
    /// 4. Sync blobs referenced by all documents
    async fn sync_with_peer(
        endpoint: &Endpoint,
        store: &Arc<Store>,
        peer_node_id: &NodeId,
    ) -> Result<usize> {
        debug!(peer = %peer_node_id, "Connecting to peer for sync");

        let node_addr = NodeAddr::new(*peer_node_id);

        // Connect to the peer
        let conn = endpoint
            .connect(node_addr, ALPN)
            .await
            .map_err(|e| Error::sync(format!("failed to connect to peer: {e}")))?;

        debug!(peer = %peer_node_id, "Connected to peer");

        // Step 1: Request remote doc list for discovery
        let remote_docs = match request_doc_list(&conn).await {
            Ok(docs) => {
                debug!(peer = %peer_node_id, count = docs.len(), "Received remote doc list");
                docs
            }
            Err(e) => {
                warn!(peer = %peer_node_id, error = %e, "Failed to get remote doc list, syncing local only");
                Vec::new()
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

        // Step 3: Sync each document
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

        // Step 4: Sync blobs referenced by all documents (including newly discovered ones)
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

        Ok(synced_count)
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
        node.registry().add(device_info);

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
        node.registry().add(device_info);

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
