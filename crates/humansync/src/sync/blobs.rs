//! Blob synchronization via iroh-blobs
//!
//! Large files are stored and transferred using iroh-blobs, which provides:
//! - Content-addressed storage (deduplication)
//! - Efficient transfer (only missing chunks)
//! - Resume after disconnect
//!
//! This module provides the `BlobStore` struct that wraps iroh-blobs' FsStore
//! for persistent, content-addressed blob storage.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use bytes::Bytes;
use iroh::{Endpoint, NodeAddr};
use iroh_blobs::store::fs::FsStore;
use iroh_blobs::Hash;
use parking_lot::RwLock;
use tokio::io::AsyncReadExt;
use tracing::{debug, info, warn};

use crate::error::{Error, Result};

/// Tracks access time and size for cache eviction
#[derive(Debug, Clone)]
struct CacheEntry {
    last_accessed: Instant,
    size: u64,
}

/// Wrapper around iroh-blobs FsStore for content-addressed blob storage
///
/// This provides a high-level API for storing and retrieving blobs using
/// iroh-blobs' content-addressed storage system with BLAKE3 hashing.
pub struct BlobStore {
    /// The underlying iroh-blobs file store
    store: FsStore,
    /// Root path for the blob store
    root_path: PathBuf,
    /// Path for exported blobs (when retrieving by hash)
    export_path: PathBuf,
    /// LRU cache tracking for exported blobs
    cache: RwLock<HashMap<String, CacheEntry>>,
    /// Maximum cache size in bytes (None = unlimited)
    max_cache_bytes: Option<u64>,
}

impl BlobStore {
    /// Create a new blob store at the given path
    ///
    /// This will create the necessary directories and initialize the iroh-blobs
    /// FsStore for persistent storage. The optional `max_cache_bytes` parameter
    /// controls the LRU eviction limit for exported blobs.
    pub async fn new(blobs_path: impl AsRef<Path>, max_cache_bytes: Option<u64>) -> Result<Self> {
        let root_path = blobs_path.as_ref().to_path_buf();
        let export_path = root_path.join("exports");

        // Create directories
        tokio::fs::create_dir_all(&root_path)
            .await
            .map_err(|e| Error::blob(format!("failed to create blobs directory: {e}")))?;
        tokio::fs::create_dir_all(&export_path)
            .await
            .map_err(|e| Error::blob(format!("failed to create exports directory: {e}")))?;

        // Initialize iroh-blobs FsStore
        let store = FsStore::load(&root_path)
            .await
            .map_err(|e| Error::blob(format!("failed to initialize blob store: {e}")))?;

        info!(path = %root_path.display(), ?max_cache_bytes, "Blob store initialized");

        Ok(Self {
            store,
            root_path,
            export_path,
            cache: RwLock::new(HashMap::new()),
            max_cache_bytes,
        })
    }

    /// Store a file as a blob and return its BLAKE3 hash
    ///
    /// The file will be stored in the content-addressed store. If the same
    /// content already exists, this is a no-op and returns the existing hash.
    pub async fn store_file(&self, source_path: impl AsRef<Path>) -> Result<String> {
        let source = source_path.as_ref();

        // Verify source file exists
        if !source.exists() {
            return Err(Error::blob(format!(
                "source file does not exist: {}",
                source.display()
            )));
        }

        debug!(path = %source.display(), "Storing file as blob");

        // Add the file to the blob store
        let tag = self
            .store
            .blobs()
            .add_path(source)
            .await
            .map_err(|e| Error::blob(format!("failed to add file to blob store: {e}")))?;

        let hash = tag.hash;
        let hash_str = hash.to_string();

        info!(hash = %hash_str, path = %source.display(), "File stored as blob");

        Ok(hash_str)
    }

    /// Store bytes directly as a blob and return its BLAKE3 hash
    ///
    /// Useful for storing data that's already in memory.
    pub async fn store_bytes(&self, data: impl Into<Bytes>) -> Result<String> {
        let bytes: Bytes = data.into();
        let size = bytes.len();

        debug!(size, "Storing bytes as blob");

        let tag = self
            .store
            .blobs()
            .add_bytes(bytes)
            .await
            .map_err(|e| Error::blob(format!("failed to add bytes to blob store: {e}")))?;

        let hash = tag.hash;
        let hash_str = hash.to_string();

        info!(hash = %hash_str, size, "Bytes stored as blob");

        Ok(hash_str)
    }

    /// Retrieve a blob by its hash
    ///
    /// Returns the path to the blob file. The file is exported to the exports
    /// directory if not already present there. Touches the cache entry to
    /// update the last access time for LRU eviction.
    pub async fn get_blob(&self, hash_str: &str) -> Result<PathBuf> {
        let hash = parse_hash(hash_str)?;

        // Check if blob exists in the store
        let exists = self
            .store
            .blobs()
            .has(hash)
            .await
            .map_err(|e| Error::blob(format!("failed to check blob existence: {e}")))?;

        if !exists {
            return Err(Error::blob(format!("blob not found: {hash_str}")));
        }

        // Export path for this blob
        let export_file = self.export_path.join(hash_str);

        // If already exported, touch cache and return the path
        if export_file.exists() {
            debug!(hash = %hash_str, "Blob already exported");
            let size = self.blob_size(hash_str).await?;
            self.touch_cache(hash_str, size);
            return Ok(export_file);
        }

        // Export the blob to the exports directory
        self.store
            .blobs()
            .export(hash, &export_file)
            .await
            .map_err(|e| Error::blob(format!("failed to export blob: {e}")))?;

        debug!(hash = %hash_str, path = %export_file.display(), "Blob exported");

        // Touch cache after successful export
        let size = self.blob_size(hash_str).await?;
        self.touch_cache(hash_str, size);

        Ok(export_file)
    }

    /// Get blob content as bytes
    ///
    /// Use with caution for large blobs as this loads the entire content into memory.
    /// Touches the cache entry to update the last access time for LRU eviction.
    pub async fn get_bytes(&self, hash_str: &str) -> Result<Bytes> {
        let hash = parse_hash(hash_str)?;

        let bytes = self
            .store
            .blobs()
            .get_bytes(hash)
            .await
            .map_err(|e| Error::blob(format!("failed to get blob bytes: {e}")))?;

        // Touch cache after successful retrieval
        let size = bytes.len() as u64;
        self.touch_cache(hash_str, size);

        Ok(bytes)
    }

    /// Read blob content using an async reader
    ///
    /// More efficient for large blobs as it allows streaming reads.
    pub async fn read_blob(&self, hash_str: &str) -> Result<Vec<u8>> {
        let hash = parse_hash(hash_str)?;

        let mut reader = self.store.reader(hash);
        let mut buffer = Vec::new();
        reader
            .read_to_end(&mut buffer)
            .await
            .map_err(|e| Error::blob(format!("failed to read blob: {e}")))?;

        Ok(buffer)
    }

    /// Check if a blob exists in the store
    pub async fn has_blob(&self, hash_str: &str) -> Result<bool> {
        let hash = match parse_hash(hash_str) {
            Ok(h) => h,
            Err(_) => return Ok(false),
        };

        self.store
            .blobs()
            .has(hash)
            .await
            .map_err(|e| Error::blob(format!("failed to check blob existence: {e}")))
    }

    /// Get the size of a blob in bytes
    pub async fn blob_size(&self, hash_str: &str) -> Result<u64> {
        let hash = parse_hash(hash_str)?;

        let status = self
            .store
            .blobs()
            .status(hash)
            .await
            .map_err(|e| Error::blob(format!("failed to get blob status: {e}")))?;

        match status {
            iroh_blobs::api::blobs::BlobStatus::Complete { size } => Ok(size),
            iroh_blobs::api::blobs::BlobStatus::Partial { size, .. } => {
                Ok(size.unwrap_or(0))
            }
            iroh_blobs::api::blobs::BlobStatus::NotFound => {
                Err(Error::blob(format!("blob not found: {hash_str}")))
            }
        }
    }

    /// Get the root path of the blob store
    #[must_use]
    pub fn root_path(&self) -> &Path {
        &self.root_path
    }

    /// Get the export path for blobs
    #[must_use]
    pub fn export_path(&self) -> &Path {
        &self.export_path
    }

    /// Get a reference to the underlying iroh-blobs store
    ///
    /// This is useful for advanced operations or integration with other
    /// iroh components.
    #[must_use]
    pub fn inner(&self) -> &FsStore {
        &self.store
    }

    /// Update the last access time for a blob
    pub fn touch_cache(&self, hash: &str, size: u64) {
        self.cache.write().insert(hash.to_string(), CacheEntry {
            last_accessed: Instant::now(),
            size,
        });
    }

    /// Get total size of cached/exported blobs
    pub fn cache_size(&self) -> u64 {
        self.cache.read().values().map(|e| e.size).sum()
    }

    /// Evict least recently used exported blobs until cache is under the limit
    pub async fn evict_lru(&self) -> Result<usize> {
        let max_bytes = match self.max_cache_bytes {
            Some(max) => max,
            None => return Ok(0), // No limit set
        };

        let current_size = self.cache_size();
        if current_size <= max_bytes {
            return Ok(0);
        }

        // Collect entries sorted by access time (oldest first)
        let mut entries: Vec<(String, CacheEntry)> = self.cache.read()
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        entries.sort_by_key(|(_, e)| e.last_accessed);

        let mut freed = 0u64;
        let mut evicted = 0usize;
        let target = current_size - max_bytes;

        for (hash, entry) in entries {
            if freed >= target {
                break;
            }

            // Remove the exported file
            let export_file = self.export_path.join(&hash);
            if export_file.exists() {
                if let Err(e) = tokio::fs::remove_file(&export_file).await {
                    warn!(hash = %hash, error = %e, "Failed to evict cached blob");
                    continue;
                }
            }

            freed += entry.size;
            evicted += 1;
            self.cache.write().remove(&hash);
            debug!(hash = %hash, size = entry.size, "Evicted cached blob");
        }

        info!(evicted, freed, "LRU cache eviction complete");
        Ok(evicted)
    }

    /// List all tracked blobs with their sizes
    pub fn list_cached_blobs(&self) -> Vec<(String, u64)> {
        self.cache.read()
            .iter()
            .map(|(k, v)| (k.clone(), v.size))
            .collect()
    }
}

/// Parse a hash string into an iroh-blobs Hash
///
/// iroh-blobs supports two hash formats:
/// - 64-character lowercase hexadecimal
/// - 52-character base32 (case insensitive, no padding)
///
/// This function validates the format before attempting to parse.
fn parse_hash(hash_str: &str) -> Result<Hash> {
    // iroh-blobs supports two formats:
    // - hex: 64 characters (lowercase)
    // - base32: 52 characters (case insensitive, no padding)
    let len = hash_str.len();
    if len != 64 && len != 52 {
        return Err(Error::blob(format!(
            "invalid hash length: expected 64 (hex) or 52 (base32) chars, got {}",
            len
        )));
    }

    // For hex format, check that all characters are valid hex digits
    if len == 64 && !hash_str.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(Error::blob("invalid hash format: non-hex characters in 64-char string"));
    }

    // For base32 format, check for valid base32 characters
    // Base32 uses A-Z and 2-7 (no 0, 1, 8, 9)
    if len == 52 {
        let upper = hash_str.to_ascii_uppercase();
        if !upper
            .chars()
            .all(|c| c.is_ascii_uppercase() || (c >= '2' && c <= '7'))
        {
            return Err(Error::blob("invalid hash format: non-base32 characters in 52-char string"));
        }
    }

    hash_str
        .parse()
        .map_err(|e| Error::blob(format!("invalid hash format: {e}")))
}

/// Validate blob data against its expected hash
///
/// Uses BLAKE3 hashing to verify the content matches the expected hash.
pub fn validate_blob(data: &[u8], expected_hash: &str) -> bool {
    // iroh-blobs uses BLAKE3, so we compute the hash the same way
    let computed = blake3::hash(data);
    let computed_str = computed.to_hex().to_string();

    // Simple string comparison for 64-char hex hashes
    computed_str == expected_hash
}

/// Extract blob hash references from document content
///
/// Looks for fields that contain blob hashes (stored as strings).
/// Blob hashes are identified by their format (64-char hex string).
pub fn extract_blob_refs(doc_bytes: &[u8]) -> Vec<String> {
    // Try to parse as JSON and look for hash-like strings
    if let Ok(json_str) = std::str::from_utf8(doc_bytes) {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(json_str) {
            return extract_hashes_from_json(&value);
        }
    }

    // Fall back to regex-like search for 64-char hex strings
    extract_hashes_from_bytes(doc_bytes)
}

/// Extract hash-like strings from JSON value
fn extract_hashes_from_json(value: &serde_json::Value) -> Vec<String> {
    let mut hashes = Vec::new();
    extract_hashes_recursive(value, &mut hashes);
    // Deduplicate while preserving order
    let mut seen = std::collections::HashSet::new();
    hashes.retain(|h| seen.insert(h.clone()));
    hashes
}

/// Recursively extract hashes from JSON, handling blob-related fields specially
fn extract_hashes_recursive(value: &serde_json::Value, hashes: &mut Vec<String>) {
    match value {
        serde_json::Value::String(s) => {
            // Only add if it looks like a hash (for standalone strings)
            if is_valid_hash(s) {
                hashes.push(s.clone());
            }
        }
        serde_json::Value::Array(arr) => {
            for item in arr {
                extract_hashes_recursive(item, hashes);
            }
        }
        serde_json::Value::Object(obj) => {
            for (key, val) in obj {
                // Look specifically for blob-related fields
                let is_blob_field = key.contains("blob")
                    || key.contains("hash")
                    || key.contains("attachment")
                    || key.contains("file");

                if is_blob_field {
                    if let serde_json::Value::String(s) = val {
                        if is_valid_hash(s) {
                            hashes.push(s.clone());
                            continue; // Don't recurse, we already got the hash
                        }
                    }
                }
                // Recurse into nested values
                extract_hashes_recursive(val, hashes);
            }
        }
        _ => {}
    }
}

/// Extract 64-character hex strings from raw bytes
fn extract_hashes_from_bytes(data: &[u8]) -> Vec<String> {
    let mut hashes = Vec::new();
    let text = String::from_utf8_lossy(data);

    // Simple pattern matching for 64-char hex strings
    for word in text.split(|c: char| !c.is_ascii_hexdigit()) {
        if word.len() == 64 && word.chars().all(|c| c.is_ascii_hexdigit()) {
            hashes.push(word.to_string());
        }
    }

    hashes
}

/// Check if a string looks like a valid BLAKE3 hash
fn is_valid_hash(s: &str) -> bool {
    // BLAKE3 hashes are 64 hex characters
    // We avoid parsing as iroh Hash since that can panic on certain inputs
    s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit())
}

// =============================================================================
// Blob transfer protocol constants
// =============================================================================

/// Message type for a blob request (sent by requester)
pub const BLOB_REQUEST_MSG: u8 = 0x10;

/// Response status: blob data follows
pub const BLOB_RESPONSE_SUCCESS: u8 = 0x00;

/// Response status: blob not found on peer
pub const BLOB_RESPONSE_NOT_FOUND: u8 = 0x01;

/// Response status: error processing request
pub const BLOB_RESPONSE_ERROR: u8 = 0x02;

/// Maximum blob size for transfer (1 GB)
const MAX_BLOB_TRANSFER_SIZE: u64 = 1024 * 1024 * 1024;

// =============================================================================
// Blob sync functions
// =============================================================================

/// High-level blob sync function (local-only check)
///
/// Checks which blobs from the given hashes are missing locally.
/// For actual P2P fetching, use [`sync_blobs_with_peer`].
pub async fn sync_blobs(blob_store: &BlobStore, blob_hashes: &[String]) -> Result<()> {
    if blob_hashes.is_empty() {
        return Ok(());
    }

    debug!(count = blob_hashes.len(), "Syncing blobs");

    let mut missing = Vec::new();

    for hash in blob_hashes {
        if !blob_store.has_blob(hash).await? {
            missing.push(hash.clone());
        }
    }

    if missing.is_empty() {
        debug!("All blobs already present locally");
        return Ok(());
    }

    warn!(
        count = missing.len(),
        "Missing blobs that need to be fetched from peers (use sync_blobs_with_peer)"
    );
    for hash in &missing {
        debug!(hash, "Missing blob");
    }

    Ok(())
}

/// Sync blobs with a specific peer using the iroh-blobs protocol
///
/// Given a set of blob hashes referenced by synced documents, this function:
/// 1. Checks which blobs are missing locally
/// 2. Connects to the peer using `iroh_blobs::ALPN`
/// 3. Downloads each missing blob using iroh-blobs' `get_blob` protocol
/// 4. Verifies and stores each blob in the local blob store
///
/// Returns the number of blobs successfully fetched.
///
/// # Note
///
/// The peer must be serving blobs via `iroh_blobs::BlobsProtocol` on the
/// `iroh_blobs::ALPN` for this to work. If the peer does not support the
/// blob ALPN, connections will fail gracefully with a warning.
pub async fn sync_blobs_with_peer(
    blob_store: &BlobStore,
    endpoint: &Endpoint,
    peer_addr: &NodeAddr,
    blob_hashes: &[String],
) -> Result<usize> {
    if blob_hashes.is_empty() {
        return Ok(0);
    }

    debug!(
        count = blob_hashes.len(),
        peer = %peer_addr.node_id,
        "Checking blobs to sync from peer"
    );

    // Determine which blobs we're missing
    let mut missing = Vec::new();
    for hash_str in blob_hashes {
        if !blob_store.has_blob(hash_str).await? {
            missing.push(hash_str.clone());
        }
    }

    if missing.is_empty() {
        debug!("All blobs already present locally");
        return Ok(0);
    }

    info!(
        count = missing.len(),
        peer = %peer_addr.node_id,
        "Fetching missing blobs from peer"
    );

    let mut fetched = 0usize;
    for hash_str in &missing {
        match fetch_blob_from_peer(blob_store, endpoint, peer_addr, hash_str).await {
            Ok(_path) => {
                debug!(hash = %hash_str, "Blob fetched successfully from peer");
                fetched += 1;
            }
            Err(e) => {
                warn!(
                    hash = %hash_str,
                    peer = %peer_addr.node_id,
                    error = %e,
                    "Failed to fetch blob from peer"
                );
            }
        }
    }

    info!(fetched, total_missing = missing.len(), "Blob sync complete");
    Ok(fetched)
}

/// Fetch a specific blob from a peer using a connection-based transfer
///
/// Opens a bidirectional QUIC stream on the HumanSync ALPN connection,
/// sends a blob request with the hash, receives the blob data, verifies
/// the BLAKE3 hash, and stores it in the local blob store.
///
/// # Protocol
///
/// Request frame (sent by requester):
/// - 1 byte: message type (`0x10` = blob request)
/// - 4 bytes (big-endian): hash string length
/// - N bytes: hash string (hex)
///
/// Response frame (sent by provider):
/// - 1 byte: status (`0x00` = success, `0x01` = not found, `0x02` = error)
/// - 8 bytes (big-endian): data length (only if status == success)
/// - N bytes: blob data (only if status == success)
///
/// Returns the path to the locally stored blob.
pub async fn fetch_blob_from_peer(
    blob_store: &BlobStore,
    endpoint: &Endpoint,
    peer_addr: &NodeAddr,
    hash_str: &str,
) -> Result<PathBuf> {
    // If we already have it, return the local path
    if blob_store.has_blob(hash_str).await? {
        return blob_store.get_blob(hash_str).await;
    }

    // Validate the hash format before making the request
    let _hash = parse_hash(hash_str)?;

    debug!(
        hash = %hash_str,
        peer = %peer_addr.node_id,
        "Fetching blob from peer via connection-based transfer"
    );

    // Connect to the peer on the HumanSync ALPN
    let conn = endpoint
        .connect(peer_addr.clone(), crate::ALPN)
        .await
        .map_err(|e| Error::blob(format!(
            "failed to connect to peer {} for blob fetch: {e}",
            peer_addr.node_id
        )))?;

    // Open a bidirectional stream for the blob request
    let (mut send, mut recv) = conn
        .open_bi()
        .await
        .map_err(|e| Error::blob(format!("failed to open stream for blob request: {e}")))?;

    // Send the blob request frame
    let hash_bytes = hash_str.as_bytes();
    let hash_len = hash_bytes.len() as u32;

    // Message type: 0x10 = blob request
    send.write_all(&[BLOB_REQUEST_MSG])
        .await
        .map_err(|e| Error::blob(format!("failed to send blob request type: {e}")))?;

    // Hash string length (4 bytes big-endian)
    send.write_all(&hash_len.to_be_bytes())
        .await
        .map_err(|e| Error::blob(format!("failed to send hash length: {e}")))?;

    // Hash string
    send.write_all(hash_bytes)
        .await
        .map_err(|e| Error::blob(format!("failed to send hash: {e}")))?;

    // Signal that we're done writing the request
    send.finish()
        .map_err(|e| Error::blob(format!("failed to finish send stream: {e}")))?;

    // Read the response status
    let mut status_buf = [0u8; 1];
    recv.read_exact(&mut status_buf)
        .await
        .map_err(|e| Error::blob(format!("failed to read blob response status: {e}")))?;

    match status_buf[0] {
        BLOB_RESPONSE_SUCCESS => {
            // Read the data length (8 bytes big-endian)
            let mut len_buf = [0u8; 8];
            recv.read_exact(&mut len_buf)
                .await
                .map_err(|e| Error::blob(format!("failed to read blob data length: {e}")))?;
            let data_len = u64::from_be_bytes(len_buf);

            // Sanity check: reject unreasonably large blobs (1 GB limit)
            if data_len > MAX_BLOB_TRANSFER_SIZE {
                return Err(Error::blob(format!(
                    "blob too large for transfer: {data_len} bytes (max {MAX_BLOB_TRANSFER_SIZE})"
                )));
            }

            // Read the blob data
            let mut blob_data = vec![0u8; data_len as usize];
            recv.read_exact(&mut blob_data)
                .await
                .map_err(|e| Error::blob(format!("failed to read blob data: {e}")))?;

            // Verify the BLAKE3 hash
            if !validate_blob(&blob_data, hash_str) {
                return Err(Error::blob(format!(
                    "blob hash mismatch for {hash_str}: received data does not match expected hash"
                )));
            }

            // Store the downloaded bytes in the local blob store
            let stored_hash = blob_store.store_bytes(blob_data).await?;

            // Sanity check: the stored hash should match what we requested
            if stored_hash != hash_str {
                warn!(
                    expected = %hash_str,
                    actual = %stored_hash,
                    "Hash mismatch after storing blob (format difference?)"
                );
            }

            info!(
                hash = %hash_str,
                size = data_len,
                peer = %peer_addr.node_id,
                "Blob fetched and stored from peer"
            );

            // Return the path to the local blob
            blob_store.get_blob(hash_str).await
        }
        BLOB_RESPONSE_NOT_FOUND => {
            Err(Error::blob(format!(
                "peer {} does not have blob {hash_str}",
                peer_addr.node_id
            )))
        }
        status => {
            Err(Error::blob(format!(
                "peer {} returned error status {status:#04x} for blob {hash_str}",
                peer_addr.node_id
            )))
        }
    }
}

/// Handle an incoming blob request from a peer
///
/// This should be called when receiving a message with type `BLOB_REQUEST_MSG`
/// on a bidirectional stream. It reads the requested hash, looks up the blob
/// in the local store, and sends it back.
pub async fn handle_blob_request(
    blob_store: &BlobStore,
    send: &mut iroh::endpoint::SendStream,
    recv: &mut iroh::endpoint::RecvStream,
) -> Result<()> {
    // Read hash length (4 bytes big-endian)
    let mut len_buf = [0u8; 4];
    recv.read_exact(&mut len_buf)
        .await
        .map_err(|e| Error::blob(format!("failed to read hash length: {e}")))?;
    let hash_len = u32::from_be_bytes(len_buf) as usize;

    // Sanity check hash length
    if hash_len > 128 {
        send.write_all(&[BLOB_RESPONSE_ERROR])
            .await
            .map_err(|e| Error::blob(format!("failed to send error response: {e}")))?;
        return Err(Error::blob(format!("invalid hash length: {hash_len}")));
    }

    // Read the hash string
    let mut hash_buf = vec![0u8; hash_len];
    recv.read_exact(&mut hash_buf)
        .await
        .map_err(|e| Error::blob(format!("failed to read hash: {e}")))?;

    let hash_str = String::from_utf8(hash_buf)
        .map_err(|e| Error::blob(format!("invalid hash encoding: {e}")))?;

    debug!(hash = %hash_str, "Received blob request from peer");

    // Check if we have the blob
    if !blob_store.has_blob(&hash_str).await? {
        debug!(hash = %hash_str, "Blob not found, sending not-found response");
        send.write_all(&[BLOB_RESPONSE_NOT_FOUND])
            .await
            .map_err(|e| Error::blob(format!("failed to send not-found response: {e}")))?;
        return Ok(());
    }

    // Read the blob data
    let blob_bytes = blob_store.get_bytes(&hash_str).await?;

    // Send success response
    send.write_all(&[BLOB_RESPONSE_SUCCESS])
        .await
        .map_err(|e| Error::blob(format!("failed to send success status: {e}")))?;

    // Send data length (8 bytes big-endian)
    let data_len = blob_bytes.len() as u64;
    send.write_all(&data_len.to_be_bytes())
        .await
        .map_err(|e| Error::blob(format!("failed to send data length: {e}")))?;

    // Send the blob data
    send.write_all(&blob_bytes)
        .await
        .map_err(|e| Error::blob(format!("failed to send blob data: {e}")))?;

    // Finish the send stream
    send.finish()
        .map_err(|e| Error::blob(format!("failed to finish blob response stream: {e}")))?;

    info!(hash = %hash_str, size = data_len, "Blob sent to peer");
    Ok(())
}

/// Fetch a specific blob, checking locally first
///
/// If the blob is available locally, returns it immediately.
/// Otherwise, returns an error indicating the blob must be fetched from a peer
/// using [`fetch_blob_from_peer`].
pub async fn fetch_blob(blob_store: &BlobStore, hash: &str) -> Result<PathBuf> {
    // First check if we have it locally
    if blob_store.has_blob(hash).await? {
        return blob_store.get_blob(hash).await;
    }

    Err(Error::blob(format!(
        "blob not available locally: {hash}. Use fetch_blob_from_peer() to download from a peer."
    )))
}

/// Wrapper type for sharing BlobStore across async contexts
pub type SharedBlobStore = Arc<BlobStore>;

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    async fn create_test_store() -> (TempDir, BlobStore) {
        let temp_dir = TempDir::new().unwrap();
        let blobs_path = temp_dir.path().join("blobs");
        let store = BlobStore::new(&blobs_path, None).await.unwrap();
        (temp_dir, store)
    }

    #[test]
    fn test_validate_blob() {
        let data = b"hello world";
        let hash = blake3::hash(data).to_hex().to_string();

        assert!(validate_blob(data, &hash));
        assert!(!validate_blob(data, "wrong_hash"));
        assert!(!validate_blob(b"different data", &hash));
    }

    #[test]
    fn test_is_valid_hash() {
        // Valid 64-char hex hash (real BLAKE3 hash)
        let data = b"test data";
        let valid_hash = blake3::hash(data).to_hex().to_string();
        assert!(is_valid_hash(&valid_hash));

        // Invalid - wrong length
        assert!(!is_valid_hash("abc123"));

        // Invalid - too short
        assert!(!is_valid_hash("deadbeef"));
    }

    #[test]
    fn test_extract_hashes_from_json() {
        // Use real BLAKE3 hash format (64 hex characters)
        let hash1 = blake3::hash(b"data1").to_hex().to_string();
        let hash2 = blake3::hash(b"data2").to_hex().to_string();

        let json = serde_json::json!({
            "title": "Test Document",
            "attachment_hash": hash1,
            "nested": {
                "blob": hash2
            }
        });

        let hashes = extract_hashes_from_json(&json);
        assert_eq!(hashes.len(), 2);
        assert!(hashes.contains(&hash1));
        assert!(hashes.contains(&hash2));
    }

    #[tokio::test]
    async fn test_store_and_get_bytes() {
        let (_dir, store) = create_test_store().await;

        let data = b"test blob data";
        let hash = store.store_bytes(data.to_vec()).await.unwrap();

        assert!(!hash.is_empty());
        assert!(store.has_blob(&hash).await.unwrap());

        let retrieved = store.get_bytes(&hash).await.unwrap();
        assert_eq!(&retrieved[..], data);
    }

    #[tokio::test]
    async fn test_store_file() {
        let (temp_dir, store) = create_test_store().await;

        // Create a test file
        let test_file = temp_dir.path().join("test.txt");
        tokio::fs::write(&test_file, b"hello world from file")
            .await
            .unwrap();

        // Store it as a blob
        let hash = store.store_file(&test_file).await.unwrap();
        assert!(!hash.is_empty());

        // Retrieve and verify
        let blob_path = store.get_blob(&hash).await.unwrap();
        let content = tokio::fs::read(&blob_path).await.unwrap();
        assert_eq!(content, b"hello world from file");
    }

    #[tokio::test]
    async fn test_blob_not_found() {
        let (_dir, store) = create_test_store().await;

        let fake_hash = "a".repeat(64);
        let result = store.get_blob(&fake_hash).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_read_blob() {
        let (_dir, store) = create_test_store().await;

        let data = b"streaming read test data";
        let hash = store.store_bytes(data.to_vec()).await.unwrap();

        let read_data = store.read_blob(&hash).await.unwrap();
        assert_eq!(&read_data[..], data);
    }

    #[tokio::test]
    async fn test_blob_size() {
        let (_dir, store) = create_test_store().await;

        let data = vec![0u8; 1024]; // 1KB
        let hash = store.store_bytes(data.clone()).await.unwrap();

        let size = store.blob_size(&hash).await.unwrap();
        assert_eq!(size, 1024);
    }

    #[tokio::test]
    async fn test_deduplication() {
        let (_dir, store) = create_test_store().await;

        let data = b"duplicate content";

        // Store the same data twice
        let hash1 = store.store_bytes(data.to_vec()).await.unwrap();
        let hash2 = store.store_bytes(data.to_vec()).await.unwrap();

        // Should get the same hash (content-addressed deduplication)
        assert_eq!(hash1, hash2);
    }

    // ==========================================
    // Large file size tests
    // ==========================================

    /// Test storing and retrieving a 1KB blob
    #[tokio::test]
    async fn test_blob_1kb() {
        let (_dir, store) = create_test_store().await;

        // Generate 1KB of random-ish data
        let data: Vec<u8> = (0..1024).map(|i| (i % 256) as u8).collect();

        let start = std::time::Instant::now();
        let hash = store.store_bytes(data.clone()).await.unwrap();
        let store_duration = start.elapsed();

        assert!(store.has_blob(&hash).await.unwrap());

        let start = std::time::Instant::now();
        let retrieved = store.get_bytes(&hash).await.unwrap();
        let retrieve_duration = start.elapsed();

        assert_eq!(retrieved.len(), 1024);
        assert_eq!(&retrieved[..], &data[..]);

        let size = store.blob_size(&hash).await.unwrap();
        assert_eq!(size, 1024);

        // Log performance info
        eprintln!(
            "1KB blob: store={:?}, retrieve={:?}",
            store_duration, retrieve_duration
        );
    }

    /// Test storing and retrieving a 1MB blob
    #[tokio::test]
    async fn test_blob_1mb() {
        let (_dir, store) = create_test_store().await;

        // Generate 1MB of random-ish data
        let size = 1024 * 1024; // 1MB
        let data: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();

        let start = std::time::Instant::now();
        let hash = store.store_bytes(data.clone()).await.unwrap();
        let store_duration = start.elapsed();

        assert!(store.has_blob(&hash).await.unwrap());

        let start = std::time::Instant::now();
        let retrieved = store.get_bytes(&hash).await.unwrap();
        let retrieve_duration = start.elapsed();

        assert_eq!(retrieved.len(), size);
        assert_eq!(&retrieved[..], &data[..]);

        let blob_size = store.blob_size(&hash).await.unwrap();
        assert_eq!(blob_size, size as u64);

        // Log performance info
        eprintln!(
            "1MB blob: store={:?}, retrieve={:?}",
            store_duration, retrieve_duration
        );
    }

    /// Test storing and retrieving a 10MB blob
    #[tokio::test]
    async fn test_blob_10mb() {
        let (_dir, store) = create_test_store().await;

        // Generate 10MB of random-ish data
        let size = 10 * 1024 * 1024; // 10MB
        let data: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();

        let start = std::time::Instant::now();
        let hash = store.store_bytes(data.clone()).await.unwrap();
        let store_duration = start.elapsed();

        assert!(store.has_blob(&hash).await.unwrap());

        let start = std::time::Instant::now();
        let retrieved = store.get_bytes(&hash).await.unwrap();
        let retrieve_duration = start.elapsed();

        assert_eq!(retrieved.len(), size);
        // Verify first and last chunks match (full comparison would be slow)
        assert_eq!(&retrieved[..1024], &data[..1024]);
        assert_eq!(&retrieved[size - 1024..], &data[size - 1024..]);

        let blob_size = store.blob_size(&hash).await.unwrap();
        assert_eq!(blob_size, size as u64);

        // Log performance info
        eprintln!(
            "10MB blob: store={:?}, retrieve={:?}",
            store_duration, retrieve_duration
        );
    }

    /// Test storing a large file from disk
    #[tokio::test]
    async fn test_store_large_file_from_disk() {
        let (temp_dir, store) = create_test_store().await;

        // Create a 5MB test file
        let size = 5 * 1024 * 1024;
        let data: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();
        let test_file = temp_dir.path().join("large_test.bin");
        tokio::fs::write(&test_file, &data).await.unwrap();

        // Store as blob
        let start = std::time::Instant::now();
        let hash = store.store_file(&test_file).await.unwrap();
        let store_duration = start.elapsed();

        assert!(store.has_blob(&hash).await.unwrap());

        // Retrieve to a path
        let start = std::time::Instant::now();
        let blob_path = store.get_blob(&hash).await.unwrap();
        let retrieve_duration = start.elapsed();

        // Verify content
        let retrieved = tokio::fs::read(&blob_path).await.unwrap();
        assert_eq!(retrieved.len(), size);
        assert_eq!(&retrieved[..1024], &data[..1024]);
        assert_eq!(&retrieved[size - 1024..], &data[size - 1024..]);

        // Log performance info
        eprintln!(
            "5MB file: store={:?}, retrieve={:?}",
            store_duration, retrieve_duration
        );
    }

    /// Test streaming read for large blob
    #[tokio::test]
    async fn test_streaming_read_large_blob() {
        let (_dir, store) = create_test_store().await;

        // Store a 2MB blob
        let size = 2 * 1024 * 1024;
        let data: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();
        let hash = store.store_bytes(data.clone()).await.unwrap();

        // Read using the streaming reader
        let start = std::time::Instant::now();
        let read_data = store.read_blob(&hash).await.unwrap();
        let read_duration = start.elapsed();

        assert_eq!(read_data.len(), size);
        assert_eq!(&read_data[..1024], &data[..1024]);

        eprintln!("2MB streaming read: {:?}", read_duration);
    }

    /// Test content verification with corrupted hash
    #[tokio::test]
    async fn test_invalid_hash_format() {
        let (_dir, store) = create_test_store().await;

        // Test with invalid hash formats
        assert!(store.get_blob("invalid").await.is_err());
        assert!(store.get_blob("").await.is_err());
        assert!(store.get_blob("abc123").await.is_err());

        // Test has_blob returns false for invalid hashes
        assert!(!store.has_blob("invalid").await.unwrap());
        assert!(!store.has_blob("").await.unwrap());
        assert!(!store.has_blob("abc123").await.unwrap());
    }

    #[tokio::test]
    async fn test_cache_tracking() {
        let (_dir, store) = create_test_store().await;
        let data = b"cache test data";
        let hash = store.store_bytes(data.to_vec()).await.unwrap();

        // Initially no cache entries
        assert_eq!(store.cache_size(), 0);

        // Get blob triggers cache tracking
        let _ = store.get_blob(&hash).await.unwrap();
        assert!(store.cache_size() > 0);
    }

    #[tokio::test]
    async fn test_cache_touch_updates_time() {
        let (_dir, store) = create_test_store().await;
        let data = b"touch test";
        let hash = store.store_bytes(data.to_vec()).await.unwrap();

        store.touch_cache(&hash, 10);
        let time1 = store.cache.read().get(&hash).unwrap().last_accessed;

        std::thread::sleep(std::time::Duration::from_millis(10));
        store.touch_cache(&hash, 10);
        let time2 = store.cache.read().get(&hash).unwrap().last_accessed;

        assert!(time2 > time1);
    }

    #[tokio::test]
    async fn test_lru_eviction() {
        let temp_dir = TempDir::new().unwrap();
        let blobs_path = temp_dir.path().join("blobs");
        let store = BlobStore::new(&blobs_path, Some(100)).await.unwrap(); // 100 byte limit

        // Store several blobs
        let hash1 = store.store_bytes(vec![1u8; 50]).await.unwrap();
        let hash2 = store.store_bytes(vec![2u8; 50]).await.unwrap();
        let hash3 = store.store_bytes(vec![3u8; 50]).await.unwrap();

        // Access them (hash1 first, then hash2, then hash3)
        let _ = store.get_blob(&hash1).await.unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        let _ = store.get_blob(&hash2).await.unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        let _ = store.get_blob(&hash3).await.unwrap();

        // Cache should be over limit (150 bytes > 100)
        assert!(store.cache_size() > 100);

        // Evict
        let evicted = store.evict_lru().await.unwrap();
        assert!(evicted > 0);
        assert!(store.cache_size() <= 100);
    }

    #[tokio::test]
    async fn test_no_eviction_without_limit() {
        let (_dir, store) = create_test_store().await; // No limit

        let hash = store.store_bytes(vec![0u8; 1000]).await.unwrap();
        let _ = store.get_blob(&hash).await.unwrap();

        let evicted = store.evict_lru().await.unwrap();
        assert_eq!(evicted, 0); // No eviction when no limit set
    }
}
