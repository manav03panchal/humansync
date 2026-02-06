//! Cloud peer - always-on Iroh node

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use iroh::{Endpoint, NodeId, SecretKey};
use tokio::fs;
use tokio::sync::watch;
use tracing::{debug, info, warn};

use humansync::sync::blobs::BLOB_REQUEST_MSG;
use humansync::sync::{handle_blob_request, handle_doc_list_request, run_sync_protocol, DOC_LIST_REQUEST};
use humansync::store::Store;
use humansync::ALPN;

use crate::db::Database;

/// The cloud peer - an always-on Iroh node for sync
pub struct CloudPeer {
    endpoint: Endpoint,
    node_id: NodeId,
    store: Arc<Store>,
    shutdown_tx: watch::Sender<bool>,
}

impl CloudPeer {
    /// Create a new cloud peer
    pub async fn new(data_dir: &Path, port: u16) -> Result<Self> {
        // Load or create identity
        let secret_key = load_or_create_identity(data_dir).await?;
        let node_id = secret_key.public();

        info!(node_id = %node_id, "Cloud peer identity loaded");

        // Create Iroh endpoint
        let endpoint = Endpoint::builder()
            .secret_key(secret_key)
            .alpns(vec![ALPN.to_vec()])
            .discovery_n0()
            .discovery_local_network()
            .bind()
            .await
            .context("Failed to bind Iroh endpoint")?;

        debug!(port, "Iroh endpoint bound");

        // Initialize the Store for doc/blob persistence
        let sync_dir = data_dir.join("sync");
        let config = humansync::Config::new(&sync_dir);
        let store = Arc::new(
            Store::new(&config).context("Failed to create Store")?,
        );
        store
            .init_blob_store()
            .await
            .context("Failed to initialize blob store")?;
        info!(path = %sync_dir.display(), "Store and blob store initialized");

        // Create shutdown signal channel
        let (shutdown_tx, _shutdown_rx) = watch::channel(false);

        Ok(Self {
            endpoint,
            node_id,
            store,
            shutdown_tx,
        })
    }

    /// Get the node ID
    pub fn node_id(&self) -> NodeId {
        self.node_id
    }

    /// Get the endpoint for connection handling
    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    /// Get a reference to the sync store
    pub fn store(&self) -> &Arc<Store> {
        &self.store
    }

    /// Start the accept loop, gating incoming connections against the device registry
    ///
    /// Spawns a background task that:
    /// 1. Accepts incoming Iroh connections
    /// 2. Checks the remote NodeId against the device registry
    /// 3. If registered, spawns a handler task for the connection
    /// 4. If not registered, drops the connection
    pub fn start_accept_loop(&self, db: Arc<Database>) {
        let endpoint = self.endpoint.clone();
        let store = self.store.clone();
        let mut shutdown_rx = self.shutdown_tx.subscribe();

        tokio::spawn(async move {
            info!("Cloud peer accept loop started");

            loop {
                tokio::select! {
                    // Check for shutdown signal
                    _ = shutdown_rx.changed() => {
                        if *shutdown_rx.borrow() {
                            info!("Accept loop received shutdown signal");
                            break;
                        }
                    }
                    // Accept incoming connections
                    incoming = endpoint.accept() => {
                        let incoming = match incoming {
                            Some(incoming) => incoming,
                            None => {
                                info!("Endpoint closed, stopping accept loop");
                                break;
                            }
                        };

                        let conn = match incoming.await {
                            Ok(conn) => conn,
                            Err(e) => {
                                warn!(error = %e, "Failed to accept connection");
                                continue;
                            }
                        };

                        let remote_node_id = match conn.remote_node_id() {
                            Ok(id) => id,
                            Err(e) => {
                                warn!(error = %e, "Connection has no remote NodeId, dropping");
                                continue;
                            }
                        };

                        // Gate against device registry
                        match db.contains(&remote_node_id) {
                            Ok(true) => {
                                info!(remote = %remote_node_id, "Accepted connection from registered device");
                                // Touch device to update last_seen
                                if let Err(e) = db.touch_device(&remote_node_id) {
                                    warn!(error = %e, "Failed to touch device");
                                }
                            }
                            Ok(false) => {
                                warn!(remote = %remote_node_id, "Rejected connection from unregistered device");
                                continue; // Drop connection
                            }
                            Err(e) => {
                                warn!(error = %e, remote = %remote_node_id, "Failed to check device registry");
                                continue;
                            }
                        }

                        // Spawn handler for this connection
                        let store = store.clone();
                        tokio::spawn(async move {
                            if let Err(e) = handle_connection(conn, store).await {
                                warn!(remote = %remote_node_id, error = %e, "Connection handler error");
                            }
                        });
                    }
                }
            }

            info!("Cloud peer accept loop stopped");
        });
    }

    /// Shut down the cloud peer
    pub async fn shutdown(self) {
        info!("Shutting down cloud peer");
        // Signal the accept loop to stop
        let _ = self.shutdown_tx.send(true);
        self.endpoint.close().await;
    }
}

/// Handle an accepted connection: accept bidirectional streams and route by protocol tag
async fn handle_connection(
    conn: iroh::endpoint::Connection,
    store: Arc<Store>,
) -> Result<()> {
    let remote_str = conn
        .remote_node_id()
        .map(|id| id.to_string())
        .unwrap_or_else(|_| "unknown".to_string());
    debug!(remote = %remote_str, "Handling connection");

    // Accept multiple bidirectional streams on this connection
    loop {
        let (send, mut recv) = match conn.accept_bi().await {
            Ok(streams) => streams,
            Err(e) => {
                // Connection closed or error - this is normal when the peer is done
                debug!(remote = %remote_str, error = %e, "No more streams on connection");
                break;
            }
        };

        // Peek at the first byte to determine the request type
        let mut tag = [0u8; 1];
        match recv.read_exact(&mut tag).await {
            Ok(()) => {}
            Err(e) => {
                warn!(remote = %remote_str, error = %e, "Failed to read protocol tag");
                continue;
            }
        }

        let store = store.clone();

        match tag[0] {
            BLOB_REQUEST_MSG => {
                // Blob transfer request - handle on the current streams
                debug!(remote = %remote_str, "Routing to blob handler");
                let remote_str = remote_str.clone();
                tokio::spawn(async move {
                    let blob_store = match store.blob_store() {
                        Some(bs) => bs,
                        None => {
                            warn!("Blob store not initialized, cannot handle blob request");
                            return;
                        }
                    };
                    let mut send = send;
                    let mut recv = recv;
                    if let Err(e) =
                        handle_blob_request(&blob_store, &mut send, &mut recv).await
                    {
                        warn!(remote = %remote_str, error = %e, "Blob request handler error");
                    }
                });
            }
            DOC_LIST_REQUEST => {
                // Doc list discovery request - respond with our doc list
                debug!(remote = %remote_str, "Routing to doc list handler");
                let remote_str = remote_str.clone();
                tokio::spawn(async move {
                    let doc_names = match store.list_docs("") {
                        Ok(names) => names,
                        Err(e) => {
                            warn!(remote = %remote_str, error = %e, "Failed to list docs for discovery");
                            return;
                        }
                    };
                    if let Err(e) = handle_doc_list_request(send, &doc_names).await {
                        warn!(remote = %remote_str, error = %e, "Doc list handler error");
                    }
                });
            }
            _sync_byte => {
                // Doc sync request - the first byte is part of the doc name length prefix
                // Read the doc name header: 4-byte name length + name bytes
                debug!(remote = %remote_str, "Routing to doc sync handler");
                let remote_str = remote_str.clone();
                tokio::spawn(async move {
                    if let Err(e) =
                        handle_doc_sync(store, send, recv, tag[0]).await
                    {
                        warn!(remote = %remote_str, error = %e, "Doc sync handler error");
                    }
                });
            }
        }
    }

    Ok(())
}

/// Handle a doc sync stream
///
/// The protocol for server-side doc sync:
/// 1. Read a doc name header (4-byte length + name bytes) from the stream
/// 2. Open or create the local document from the Store
/// 3. Run the Automerge sync protocol on the existing streams
/// 4. Save the synced document
///
/// The first byte of the stream has already been read as the protocol tag.
/// For doc sync, the first byte is actually the start of the doc name length prefix.
async fn handle_doc_sync(
    store: Arc<Store>,
    send: iroh::endpoint::SendStream,
    mut recv: iroh::endpoint::RecvStream,
    first_byte: u8,
) -> Result<()> {
    // Read the remaining 3 bytes of the 4-byte doc name length
    let mut remaining_len = [0u8; 3];
    recv.read_exact(&mut remaining_len)
        .await
        .context("Failed to read doc name length")?;

    let name_len = u32::from_be_bytes([first_byte, remaining_len[0], remaining_len[1], remaining_len[2]]) as usize;

    if name_len == 0 || name_len > 4096 {
        anyhow::bail!("Invalid doc name length: {name_len}");
    }

    // Read the doc name
    let mut name_buf = vec![0u8; name_len];
    recv.read_exact(&mut name_buf)
        .await
        .context("Failed to read doc name")?;

    let doc_name = String::from_utf8(name_buf)
        .context("Invalid doc name encoding")?;

    debug!(doc = %doc_name, "Starting doc sync");

    // Open or create the local document
    let local_doc = store
        .open_doc(&doc_name)
        .await
        .context("Failed to open local document")?;

    // Run the sync protocol on the already-accepted streams
    run_sync_protocol(send, recv, &local_doc)
        .await
        .map_err(|e| anyhow::anyhow!("Sync protocol error: {e}"))?;

    // Save the synced document
    store
        .save_doc_with_index(&local_doc)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to save synced document: {e}"))?;

    info!(doc = %doc_name, "Doc sync completed and saved");
    Ok(())
}

/// Load or create the server's identity
async fn load_or_create_identity(data_dir: &Path) -> Result<SecretKey> {
    let key_path = data_dir.join("server_key");

    if key_path.exists() {
        debug!(path = %key_path.display(), "Loading server identity");

        let bytes = fs::read(&key_path)
            .await
            .context("Failed to read key file")?;

        let key_array: [u8; 32] = bytes
            .try_into()
            .map_err(|_| anyhow::anyhow!("Invalid key length"))?;

        Ok(SecretKey::from_bytes(&key_array))
    } else {
        debug!("Generating new server identity");

        let secret_key = SecretKey::generate(rand::thread_rng());

        // Ensure directory exists
        fs::create_dir_all(data_dir)
            .await
            .context("Failed to create data directory")?;

        // Write key with restrictive permissions
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;

            let key_path_clone = key_path.clone();
            let key_bytes = secret_key.to_bytes();

            tokio::task::spawn_blocking(move || {
                use std::io::Write;

                let mut file = std::fs::OpenOptions::new()
                    .write(true)
                    .create(true)
                    .truncate(true)
                    .mode(0o600)
                    .open(&key_path_clone)?;

                file.write_all(&key_bytes)?;
                file.flush()?;
                Ok::<_, std::io::Error>(())
            })
            .await
            .context("Key write task failed")?
            .context("Failed to write key file")?;
        }

        #[cfg(not(unix))]
        {
            fs::write(&key_path, secret_key.to_bytes())
                .await
                .context("Failed to write key file")?;
        }

        info!(path = %key_path.display(), "Generated new server identity");
        Ok(secret_key)
    }
}
