//! Local storage for documents and blobs
//!
//! This module provides the `Store` struct which manages both Automerge
//! documents and content-addressed blobs using iroh-blobs.
//!
//! ## Features
//!
//! - **Atomic writes**: Documents are written to a temp file first, then renamed
//!   to prevent corruption on crash.
//! - **Document index**: A special `_index` document tracks all document IDs and
//!   their last modified timestamps, enabling selective sync.
//! - **Content-addressed blobs**: Files are stored by their BLAKE3 hash using
//!   iroh-blobs for deduplication.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use automerge::{AutoCommit, ReadDoc, transaction::Transactable};
use chrono::{DateTime, Utc};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex as TokioMutex;
use tracing::{debug, info, warn};

use crate::config::Config;
use crate::doc::Document;
use crate::error::{Error, Result};
use crate::sync::blobs::BlobStore;

/// The name of the special index document
const INDEX_DOC_NAME: &str = "_index";

/// Metadata for a document in the index
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocMetadata {
    /// Last modified timestamp (ISO 8601 format)
    pub modified: String,
}

impl DocMetadata {
    /// Create new metadata with the current timestamp
    fn now() -> Self {
        Self {
            modified: Utc::now().to_rfc3339(),
        }
    }

    /// Get the modified timestamp as a DateTime
    pub fn modified_time(&self) -> Option<DateTime<Utc>> {
        DateTime::parse_from_rfc3339(&self.modified)
            .ok()
            .map(|dt| dt.with_timezone(&Utc))
    }
}

/// Document listing with metadata
#[derive(Debug, Clone)]
pub struct DocInfo {
    /// Document name/path
    pub name: String,
    /// Last modified timestamp
    pub modified: Option<DateTime<Utc>>,
}

/// Local storage for documents and blobs
///
/// The Store manages:
/// - Automerge documents stored as files in `{storage_path}/docs/`
/// - Content-addressed blobs stored via iroh-blobs in `{storage_path}/blobs/`
/// - A special `_index` document that tracks all document IDs and timestamps
///
/// ## Atomic Writes
///
/// All document writes are atomic: data is first written to a `.tmp` file,
/// then renamed to the final path. This prevents corruption if the process
/// crashes during a write.
///
/// ## Document Index
///
/// The `_index` document (stored at `docs/_index.automerge`) maintains a map
/// of all document IDs to their last modified timestamps. This enables
/// selective sync: sync the index first, then only fetch changed documents.
pub struct Store {
    /// Path to the documents directory
    docs_path: PathBuf,
    /// Path to the blobs directory
    blobs_path: PathBuf,
    /// The iroh-blobs store for content-addressed blob storage
    blob_store: RwLock<Option<Arc<BlobStore>>>,
    /// Maximum blob cache size in bytes (from config)
    max_blob_cache_bytes: Option<u64>,
    /// Mutex to serialize index file operations (read-modify-write)
    index_lock: TokioMutex<()>,
}

impl Store {
    /// Create a new store from config
    ///
    /// This creates the directory structure but does NOT initialize the blob store.
    /// Call `init_blob_store()` to initialize the async blob storage.
    pub fn new(config: &Config) -> Result<Self> {
        let docs_path = config.docs_path();
        let blobs_path = config.blobs_path();

        // Create directories synchronously for simplicity
        std::fs::create_dir_all(&docs_path)
            .map_err(|e| Error::storage(format!("failed to create docs directory: {e}")))?;
        std::fs::create_dir_all(&blobs_path)
            .map_err(|e| Error::storage(format!("failed to create blobs directory: {e}")))?;

        Ok(Self {
            docs_path,
            blobs_path,
            blob_store: RwLock::new(None),
            max_blob_cache_bytes: config.max_blob_cache_bytes,
            index_lock: TokioMutex::new(()),
        })
    }

    /// Initialize the async blob store
    ///
    /// This should be called after creating the Store to enable blob operations.
    pub async fn init_blob_store(&self) -> Result<()> {
        let blob_store = BlobStore::new(&self.blobs_path, self.max_blob_cache_bytes).await?;
        *self.blob_store.write() = Some(Arc::new(blob_store));
        info!(path = %self.blobs_path.display(), "Blob store initialized");
        Ok(())
    }

    /// Get a reference to the blob store
    ///
    /// Returns None if the blob store hasn't been initialized.
    pub fn blob_store(&self) -> Option<Arc<BlobStore>> {
        self.blob_store.read().clone()
    }

    /// Get the blob store, returning an error if not initialized
    fn require_blob_store(&self) -> Result<Arc<BlobStore>> {
        self.blob_store
            .read()
            .clone()
            .ok_or_else(|| Error::blob("blob store not initialized"))
    }

    /// Open or create a document
    ///
    /// If the document doesn't exist, it will be created and the index will be updated.
    /// For existing documents, only load is performed (index update happens on save).
    pub async fn open_doc(&self, name: &str) -> Result<Document> {
        // Don't allow opening the index document directly through this method
        // to prevent accidental corruption
        if name == INDEX_DOC_NAME {
            return Err(Error::document(
                "cannot open _index document directly; use index-specific methods",
            ));
        }

        let path = self.doc_path(name);
        debug!(name, path = %path.display(), "Opening document");

        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| Error::storage(format!("failed to create doc directory: {e}")))?;
        }

        let is_new = !path.exists();
        let doc = if !is_new {
            // Load existing document
            let bytes = tokio::fs::read(&path)
                .await
                .map_err(|e| Error::storage(format!("failed to read document: {e}")))?;
            AutoCommit::load(&bytes)
                .map_err(|e| Error::storage(format!("failed to load document: {e}")))?
        } else {
            // Create new document
            AutoCommit::new()
        };

        let document = Document::new(name.to_string(), path, doc);

        // If this is a new document, update the index
        if is_new {
            // Save the new document with atomic write
            self.save_doc_atomic(&document).await?;
            // Update the index
            self.update_index_entry(name).await?;
        }

        Ok(document)
    }

    /// Save a document atomically
    ///
    /// Writes to a temp file first, then renames to prevent corruption on crash.
    pub async fn save_doc_atomic(&self, doc: &Document) -> Result<()> {
        let path = doc.path();
        let temp_path = path.with_extension("automerge.tmp");

        debug!(path = %path.display(), "Saving document atomically");

        // Get the document bytes
        let bytes = doc.save_bytes();

        // Write to temp file
        tokio::fs::write(&temp_path, &bytes)
            .await
            .map_err(|e| Error::storage(format!("failed to write temp file: {e}")))?;

        // Atomic rename
        tokio::fs::rename(&temp_path, path)
            .await
            .map_err(|e| Error::storage(format!("failed to rename temp file: {e}")))?;

        debug!(path = %path.display(), "Document saved atomically");
        Ok(())
    }

    /// Save a document and update its entry in the index
    ///
    /// This combines atomic save with index update for convenience.
    pub async fn save_doc_with_index(&self, doc: &Document) -> Result<()> {
        self.save_doc_atomic(doc).await?;
        self.update_index_entry(doc.name()).await?;
        Ok(())
    }

    /// List document names with the given prefix
    ///
    /// This returns just the document names. For metadata including modification
    /// times, use `list_docs_with_metadata()`.
    pub fn list_docs(&self, prefix: &str) -> Result<Vec<String>> {
        let mut docs = Vec::new();
        self.list_docs_recursive(&self.docs_path, prefix, &mut docs)?;
        Ok(docs)
    }

    /// List documents with metadata from the index
    ///
    /// Returns document names along with their last modified timestamps from
    /// the index. Documents not in the index will have `None` for their
    /// modification time.
    pub async fn list_docs_with_metadata(&self, prefix: &str) -> Result<Vec<DocInfo>> {
        // Get the index data
        let index = self.load_index().await?;

        // Get all document names
        let names = self.list_docs(prefix)?;

        // Build DocInfo with metadata from index
        let docs = names
            .into_iter()
            .map(|name| {
                let modified = index
                    .get(&name)
                    .and_then(|meta| meta.modified_time());
                DocInfo { name, modified }
            })
            .collect();

        Ok(docs)
    }

    /// Delete a document from disk and update the index
    ///
    /// Returns `Ok(())` if the document was deleted or didn't exist.
    pub async fn delete_doc(&self, name: &str) -> Result<()> {
        // Don't allow deleting the index document
        if name == INDEX_DOC_NAME {
            return Err(Error::document("cannot delete the _index document"));
        }

        let path = self.doc_path(name);
        debug!(name, path = %path.display(), "Deleting document");

        // Delete the file if it exists
        if path.exists() {
            tokio::fs::remove_file(&path)
                .await
                .map_err(|e| Error::storage(format!("failed to delete document: {e}")))?;
            info!(name, "Document deleted");
        } else {
            debug!(name, "Document doesn't exist, nothing to delete");
        }

        // Also clean up any temp file that might exist
        let temp_path = path.with_extension("automerge.tmp");
        if temp_path.exists() {
            let _ = tokio::fs::remove_file(&temp_path).await;
        }

        // Remove from index
        self.remove_index_entry(name).await?;

        Ok(())
    }

    // =========================================================================
    // Index Management
    // =========================================================================

    /// Get the path for the index document
    fn index_path(&self) -> PathBuf {
        self.docs_path.join(format!("{INDEX_DOC_NAME}.automerge"))
    }

    /// Load the document index
    ///
    /// Returns a map of document names to their metadata.
    /// If the index doesn't exist, returns an empty map.
    pub async fn load_index(&self) -> Result<HashMap<String, DocMetadata>> {
        let path = self.index_path();

        if !path.exists() {
            debug!("Index document doesn't exist, returning empty index");
            return Ok(HashMap::new());
        }

        let bytes = tokio::fs::read(&path)
            .await
            .map_err(|e| Error::storage(format!("failed to read index: {e}")))?;

        let doc = match AutoCommit::load(&bytes) {
            Ok(doc) => doc,
            Err(e) => {
                warn!("Index file corrupted during load, recreating: {e}");
                let _ = tokio::fs::remove_file(&path).await;
                return Ok(HashMap::new());
            }
        };

        // Parse the "docs" map from the index
        self.parse_index_doc(&doc)
    }

    /// Parse the index document into a HashMap
    fn parse_index_doc(&self, doc: &AutoCommit) -> Result<HashMap<String, DocMetadata>> {
        let mut index = HashMap::new();

        // Get the "docs" object
        let docs_obj = match doc.get(automerge::ROOT, "docs") {
            Ok(Some((automerge::Value::Object(automerge::ObjType::Map), obj_id))) => obj_id,
            Ok(_) => return Ok(index), // No docs map or wrong type
            Err(e) => return Err(Error::storage(format!("failed to get docs map: {e}"))),
        };

        // Collect keys first to avoid borrowing issues
        let keys: Vec<String> = doc.keys(&docs_obj).map(|k| k.to_string()).collect();

        // Iterate over all keys in the docs map
        for key in keys {
            // Get the metadata object for this document
            if let Ok(Some((automerge::Value::Object(automerge::ObjType::Map), meta_obj))) =
                doc.get(&docs_obj, &key)
            {
                // Get the "modified" field
                if let Ok(Some((automerge::Value::Scalar(s), _))) = doc.get(meta_obj, "modified") {
                    if let automerge::ScalarValue::Str(modified) = s.as_ref() {
                        index.insert(
                            key,
                            DocMetadata {
                                modified: modified.to_string(),
                            },
                        );
                    }
                }
            }
        }

        Ok(index)
    }

    /// Update an entry in the index document
    ///
    /// Creates the index if it doesn't exist.
    pub async fn update_index_entry(&self, doc_name: &str) -> Result<()> {
        debug!(doc_name, "Updating index entry");

        // Serialize index read-modify-write to prevent concurrent corruption
        let _guard = self.index_lock.lock().await;

        let path = self.index_path();
        let temp_path = path.with_extension("automerge.tmp");

        // Load or create the index document (self-healing on corruption)
        let mut doc = if path.exists() {
            let bytes = tokio::fs::read(&path)
                .await
                .map_err(|e| Error::storage(format!("failed to read index: {e}")))?;
            match AutoCommit::load(&bytes) {
                Ok(doc) => doc,
                Err(e) => {
                    warn!("Index file corrupted, recreating: {e}");
                    let _ = tokio::fs::remove_file(&path).await;
                    AutoCommit::new()
                }
            }
        } else {
            AutoCommit::new()
        };

        // Ensure the "docs" map exists
        let docs_obj = match doc.get(automerge::ROOT, "docs") {
            Ok(Some((automerge::Value::Object(automerge::ObjType::Map), obj_id))) => obj_id,
            _ => {
                // Create the docs map
                doc.put_object(automerge::ROOT, "docs", automerge::ObjType::Map)
                    .map_err(|e| Error::storage(format!("failed to create docs map: {e}")))?
            }
        };

        // Create or update the entry for this document
        let entry_obj = match doc.get(&docs_obj, doc_name) {
            Ok(Some((automerge::Value::Object(automerge::ObjType::Map), obj_id))) => obj_id,
            _ => {
                // Create new entry
                doc.put_object(&docs_obj, doc_name, automerge::ObjType::Map)
                    .map_err(|e| Error::storage(format!("failed to create index entry: {e}")))?
            }
        };

        // Update the modified timestamp
        let metadata = DocMetadata::now();
        doc.put(entry_obj, "modified", metadata.modified.clone())
            .map_err(|e| Error::storage(format!("failed to update modified time: {e}")))?;

        // Save atomically
        let bytes = doc.save();
        tokio::fs::write(&temp_path, &bytes)
            .await
            .map_err(|e| Error::storage(format!("failed to write index temp file: {e}")))?;

        tokio::fs::rename(&temp_path, &path)
            .await
            .map_err(|e| Error::storage(format!("failed to rename index temp file: {e}")))?;

        debug!(doc_name, "Index entry updated");
        Ok(())
    }

    /// Remove an entry from the index document
    pub async fn remove_index_entry(&self, doc_name: &str) -> Result<()> {
        debug!(doc_name, "Removing index entry");

        // Serialize index read-modify-write to prevent concurrent corruption
        let _guard = self.index_lock.lock().await;

        let path = self.index_path();

        // If index doesn't exist, nothing to do
        if !path.exists() {
            return Ok(());
        }

        let temp_path = path.with_extension("automerge.tmp");

        let bytes = tokio::fs::read(&path)
            .await
            .map_err(|e| Error::storage(format!("failed to read index: {e}")))?;

        let mut doc = match AutoCommit::load(&bytes) {
            Ok(doc) => doc,
            Err(e) => {
                warn!("Index file corrupted during remove, recreating: {e}");
                let _ = tokio::fs::remove_file(&path).await;
                return Ok(());
            }
        };

        // Get the "docs" map
        if let Ok(Some((automerge::Value::Object(automerge::ObjType::Map), docs_obj))) =
            doc.get(automerge::ROOT, "docs")
        {
            // Delete the entry if it exists
            if doc.get(&docs_obj, doc_name).ok().flatten().is_some() {
                doc.delete(&docs_obj, doc_name)
                    .map_err(|e| Error::storage(format!("failed to delete index entry: {e}")))?;

                // Save atomically
                let bytes = doc.save();
                tokio::fs::write(&temp_path, &bytes)
                    .await
                    .map_err(|e| Error::storage(format!("failed to write index temp file: {e}")))?;

                tokio::fs::rename(&temp_path, &path)
                    .await
                    .map_err(|e| {
                        Error::storage(format!("failed to rename index temp file: {e}"))
                    })?;

                debug!(doc_name, "Index entry removed");
            }
        }

        Ok(())
    }

    /// Get the raw index document for syncing
    ///
    /// Returns the index as a Document that can be synced with peers.
    pub async fn get_index_doc(&self) -> Result<Document> {
        let path = self.index_path();

        let doc = if path.exists() {
            let bytes = tokio::fs::read(&path)
                .await
                .map_err(|e| Error::storage(format!("failed to read index: {e}")))?;
            match AutoCommit::load(&bytes) {
                Ok(doc) => doc,
                Err(e) => {
                    warn!("Index file corrupted in get_index_doc, recreating: {e}");
                    let _ = tokio::fs::remove_file(&path).await;
                    AutoCommit::new()
                }
            }
        } else {
            AutoCommit::new()
        };

        Ok(Document::new(INDEX_DOC_NAME.to_string(), path, doc))
    }

    // =========================================================================
    // Helper Methods
    // =========================================================================

    /// Recursively list documents
    fn list_docs_recursive(
        &self,
        dir: &Path,
        prefix: &str,
        docs: &mut Vec<String>,
    ) -> Result<()> {
        if !dir.exists() {
            return Ok(());
        }

        let entries = std::fs::read_dir(dir)
            .map_err(|e| Error::storage(format!("failed to read directory: {e}")))?;

        for entry in entries {
            let entry = entry
                .map_err(|e| Error::storage(format!("failed to read directory entry: {e}")))?;
            let path = entry.path();

            if path.is_dir() {
                self.list_docs_recursive(&path, prefix, docs)?;
            } else if let Some(name) = self.path_to_doc_name(&path) {
                // Skip the index document and temp files
                if name != INDEX_DOC_NAME && !name.ends_with(".tmp") && name.starts_with(prefix) {
                    docs.push(name);
                }
            }
        }

        Ok(())
    }

    /// Convert a file path to a document name
    fn path_to_doc_name(&self, path: &Path) -> Option<String> {
        let relative = path.strip_prefix(&self.docs_path).ok()?;
        let name = relative.to_str()?;
        // Remove file extension if present
        Some(name.trim_end_matches(".automerge").to_string())
    }

    /// Get the path for a document
    fn doc_path(&self, name: &str) -> PathBuf {
        self.docs_path.join(format!("{name}.automerge"))
    }

    /// Store a file as a blob and return its content hash
    ///
    /// Uses iroh-blobs for content-addressed storage with BLAKE3 hashing.
    /// If the blob store is not initialized, falls back to simple file-based storage.
    pub async fn store_blob(&self, source_path: &Path) -> Result<String> {
        // Try to use the iroh-blobs store if available
        if let Some(blob_store) = self.blob_store() {
            return blob_store.store_file(source_path).await;
        }

        // Fallback to simple file-based storage (for backward compatibility)
        debug!("Using fallback blob storage (iroh-blobs not initialized)");

        // Read the file
        let data = tokio::fs::read(source_path)
            .await
            .map_err(|e| Error::blob(format!("failed to read file: {e}")))?;

        // Compute hash (using blake3 for speed)
        let hash = blake3::hash(&data);
        let hash_str = hash.to_hex().to_string();

        // Store blob by hash
        let blob_path = self.blobs_path.join(&hash_str);
        if !blob_path.exists() {
            tokio::fs::write(&blob_path, &data)
                .await
                .map_err(|e| Error::blob(format!("failed to write blob: {e}")))?;
        }

        debug!(hash = %hash_str, "Stored blob (fallback)");
        Ok(hash_str)
    }

    /// Store bytes as a blob and return its content hash
    ///
    /// Uses iroh-blobs for content-addressed storage with BLAKE3 hashing.
    pub async fn store_blob_bytes(&self, data: impl Into<bytes::Bytes>) -> Result<String> {
        let blob_store = self.require_blob_store()?;
        blob_store.store_bytes(data).await
    }

    /// Get a blob by its hash
    ///
    /// Returns the path to the blob file. If using iroh-blobs, the blob
    /// is exported to a file that can be read.
    pub async fn get_blob(&self, hash: &str) -> Result<PathBuf> {
        // Try to use the iroh-blobs store if available
        if let Some(blob_store) = self.blob_store() {
            return blob_store.get_blob(hash).await;
        }

        // Fallback to simple file-based lookup
        let blob_path = self.blobs_path.join(hash);

        if blob_path.exists() {
            Ok(blob_path)
        } else {
            Err(Error::blob(format!("blob not found: {hash}")))
        }
    }

    /// Get blob content as bytes
    ///
    /// Use with caution for large blobs as this loads everything into memory.
    pub async fn get_blob_bytes(&self, hash: &str) -> Result<bytes::Bytes> {
        let blob_store = self.require_blob_store()?;
        blob_store.get_bytes(hash).await
    }

    /// Check if a blob exists locally
    pub async fn has_blob(&self, hash: &str) -> Result<bool> {
        if let Some(blob_store) = self.blob_store() {
            return blob_store.has_blob(hash).await;
        }

        // Fallback
        Ok(self.blobs_path.join(hash).exists())
    }

    /// Check if a blob exists (synchronous fallback check)
    ///
    /// This only checks the fallback storage path, not the iroh-blobs store.
    pub fn has_blob_sync(&self, hash: &str) -> bool {
        self.blobs_path.join(hash).exists()
    }

    /// Get the size of a blob in bytes
    pub async fn blob_size(&self, hash: &str) -> Result<u64> {
        let blob_store = self.require_blob_store()?;
        blob_store.blob_size(hash).await
    }

    /// Get the blobs directory path
    #[must_use]
    pub fn blobs_path(&self) -> &Path {
        &self.blobs_path
    }

    /// Get the docs directory path
    #[must_use]
    pub fn docs_path(&self) -> &Path {
        &self.docs_path
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn create_test_store() -> (TempDir, Store) {
        let temp_dir = TempDir::new().unwrap();
        let config = Config::new(temp_dir.path());
        let store = Store::new(&config).unwrap();
        (temp_dir, store)
    }

    async fn create_test_store_with_blobs() -> (TempDir, Store) {
        let temp_dir = TempDir::new().unwrap();
        let config = Config::new(temp_dir.path());
        let store = Store::new(&config).unwrap();
        store.init_blob_store().await.unwrap();
        (temp_dir, store)
    }

    #[tokio::test]
    async fn test_open_new_doc() {
        let (_dir, store) = create_test_store();

        let doc = store.open_doc("test/doc1").await.unwrap();
        assert_eq!(doc.name(), "test/doc1");
    }

    #[tokio::test]
    async fn test_open_existing_doc() {
        let (_dir, store) = create_test_store();

        // Create and save a document
        let doc1 = store.open_doc("test/doc1").await.unwrap();
        doc1.put("key", "value").unwrap();
        doc1.save().unwrap();

        // Open the same document
        let doc2 = store.open_doc("test/doc1").await.unwrap();
        let value: Option<String> = doc2.get("key").unwrap();
        assert_eq!(value, Some("value".to_string()));
    }

    #[tokio::test]
    async fn test_list_docs() {
        let (_dir, store) = create_test_store();

        // Create some documents
        let doc1 = store.open_doc("notes/a").await.unwrap();
        doc1.save().unwrap();
        let doc2 = store.open_doc("notes/b").await.unwrap();
        doc2.save().unwrap();
        let doc3 = store.open_doc("tasks/c").await.unwrap();
        doc3.save().unwrap();

        // List all notes
        let mut notes = store.list_docs("notes/").unwrap();
        notes.sort();
        assert_eq!(notes, vec!["notes/a", "notes/b"]);

        // List all
        let all = store.list_docs("").unwrap();
        assert_eq!(all.len(), 3);
    }

    #[tokio::test]
    async fn test_store_and_get_blob_fallback() {
        let (temp_dir, store) = create_test_store();

        // Create a test file
        let test_file = temp_dir.path().join("test.txt");
        tokio::fs::write(&test_file, b"hello world").await.unwrap();

        // Store it as a blob (fallback mode - blob store not initialized)
        let hash = store.store_blob(&test_file).await.unwrap();
        assert!(!hash.is_empty());

        // Retrieve the blob
        let blob_path = store.get_blob(&hash).await.unwrap();
        let content = tokio::fs::read(&blob_path).await.unwrap();
        assert_eq!(content, b"hello world");

        // Check has_blob
        assert!(store.has_blob(&hash).await.unwrap());
        assert!(!store.has_blob("nonexistent").await.unwrap());
    }

    #[tokio::test]
    async fn test_store_and_get_blob_iroh() {
        let (temp_dir, store) = create_test_store_with_blobs().await;

        // Create a test file
        let test_file = temp_dir.path().join("test.txt");
        tokio::fs::write(&test_file, b"hello world via iroh-blobs")
            .await
            .unwrap();

        // Store it as a blob using iroh-blobs
        let hash = store.store_blob(&test_file).await.unwrap();
        assert!(!hash.is_empty());

        // Retrieve the blob
        let blob_path = store.get_blob(&hash).await.unwrap();
        let content = tokio::fs::read(&blob_path).await.unwrap();
        assert_eq!(content, b"hello world via iroh-blobs");

        // Check has_blob
        assert!(store.has_blob(&hash).await.unwrap());
        assert!(!store.has_blob("nonexistent").await.unwrap());
    }

    #[tokio::test]
    async fn test_store_blob_bytes() {
        let (_dir, store) = create_test_store_with_blobs().await;

        // Store bytes directly
        let data = b"direct bytes storage test";
        let hash = store.store_blob_bytes(data.to_vec()).await.unwrap();
        assert!(!hash.is_empty());

        // Retrieve as bytes
        let retrieved = store.get_blob_bytes(&hash).await.unwrap();
        assert_eq!(&retrieved[..], data);
    }

    #[tokio::test]
    async fn test_blob_size() {
        let (_dir, store) = create_test_store_with_blobs().await;

        // Store some data
        let data = vec![0u8; 2048]; // 2KB
        let hash = store.store_blob_bytes(data).await.unwrap();

        // Check size
        let size = store.blob_size(&hash).await.unwrap();
        assert_eq!(size, 2048);
    }

    #[tokio::test]
    async fn test_blob_store_not_initialized_error() {
        let (_dir, store) = create_test_store();

        // Trying to use blob_bytes without initialization should fail
        let result = store.store_blob_bytes(b"test".to_vec()).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_blob_deduplication() {
        let (temp_dir, store) = create_test_store_with_blobs().await;

        // Create two files with the same content
        let file1 = temp_dir.path().join("file1.txt");
        let file2 = temp_dir.path().join("file2.txt");
        let content = b"identical content for deduplication test";

        tokio::fs::write(&file1, content).await.unwrap();
        tokio::fs::write(&file2, content).await.unwrap();

        // Store both
        let hash1 = store.store_blob(&file1).await.unwrap();
        let hash2 = store.store_blob(&file2).await.unwrap();

        // Should get the same hash (content-addressed deduplication)
        assert_eq!(hash1, hash2);
    }

    // =========================================================================
    // Tests for atomic writes
    // =========================================================================

    #[tokio::test]
    async fn test_atomic_write_creates_no_temp_file() {
        let (_dir, store) = create_test_store();

        // Create and save a document atomically
        let doc = store.open_doc("test/atomic").await.unwrap();
        doc.put("key", "value").unwrap();
        store.save_doc_atomic(&doc).await.unwrap();

        // Verify the temp file doesn't exist after save
        let temp_path = doc.path().with_extension("automerge.tmp");
        assert!(!temp_path.exists(), "Temp file should not exist after atomic save");

        // Verify the actual file exists
        assert!(doc.path().exists(), "Document file should exist");
    }

    #[tokio::test]
    async fn test_atomic_write_preserves_data() {
        let (_dir, store) = create_test_store();

        // Create and save a document with data
        let doc = store.open_doc("test/atomic-data").await.unwrap();
        doc.put("name", "test").unwrap();
        doc.put("count", 42i64).unwrap();
        store.save_doc_atomic(&doc).await.unwrap();

        // Reopen and verify data
        let doc2 = store.open_doc("test/atomic-data").await.unwrap();
        let name: Option<String> = doc2.get("name").unwrap();
        let count: Option<i64> = doc2.get("count").unwrap();
        assert_eq!(name, Some("test".to_string()));
        assert_eq!(count, Some(42));
    }

    #[tokio::test]
    async fn test_save_doc_with_index_updates_both() {
        let (_dir, store) = create_test_store();

        // Create a document and save with index
        let doc = store.open_doc("test/indexed").await.unwrap();
        doc.put("data", "value").unwrap();
        store.save_doc_with_index(&doc).await.unwrap();

        // Verify index was updated
        let index = store.load_index().await.unwrap();
        assert!(index.contains_key("test/indexed"), "Document should be in index");
        assert!(index["test/indexed"].modified_time().is_some(), "Should have valid timestamp");
    }

    // =========================================================================
    // Tests for document index
    // =========================================================================

    #[tokio::test]
    async fn test_index_created_on_new_doc() {
        let (_dir, store) = create_test_store();

        // Create a new document (should update index automatically)
        let _doc = store.open_doc("test/new-doc").await.unwrap();

        // Verify index entry exists
        let index = store.load_index().await.unwrap();
        assert!(index.contains_key("test/new-doc"), "New doc should be in index");
    }

    #[tokio::test]
    async fn test_index_not_updated_on_existing_doc() {
        let (_dir, store) = create_test_store();

        // Create a document
        let doc1 = store.open_doc("test/existing").await.unwrap();
        doc1.put("key", "value").unwrap();
        store.save_doc_atomic(&doc1).await.unwrap();

        // Get the initial modified time
        let index1 = store.load_index().await.unwrap();
        let modified1 = index1["test/existing"].modified.clone();

        // Wait a tiny bit to ensure time passes
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        // Reopen the same document (should NOT update index)
        let _doc2 = store.open_doc("test/existing").await.unwrap();

        // Verify modified time hasn't changed
        let index2 = store.load_index().await.unwrap();
        let modified2 = index2["test/existing"].modified.clone();
        assert_eq!(modified1, modified2, "Reopening existing doc should not update index");
    }

    #[tokio::test]
    async fn test_update_index_entry() {
        let (_dir, store) = create_test_store();

        // Manually update index entry
        store.update_index_entry("manual/doc").await.unwrap();

        let index = store.load_index().await.unwrap();
        assert!(index.contains_key("manual/doc"));

        // Update again
        let old_modified = index["manual/doc"].modified.clone();
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        store.update_index_entry("manual/doc").await.unwrap();

        let index2 = store.load_index().await.unwrap();
        assert_ne!(old_modified, index2["manual/doc"].modified, "Modified time should be updated");
    }

    #[tokio::test]
    async fn test_index_structure() {
        let (_dir, store) = create_test_store();

        // Create multiple documents
        let _doc1 = store.open_doc("namespace/doc1").await.unwrap();
        let _doc2 = store.open_doc("namespace/doc2").await.unwrap();
        let _doc3 = store.open_doc("other/doc3").await.unwrap();

        // Load and verify index
        let index = store.load_index().await.unwrap();
        assert_eq!(index.len(), 3);
        assert!(index.contains_key("namespace/doc1"));
        assert!(index.contains_key("namespace/doc2"));
        assert!(index.contains_key("other/doc3"));
    }

    #[tokio::test]
    async fn test_load_index_when_missing() {
        let (_dir, store) = create_test_store();

        // Load index when it doesn't exist
        let index = store.load_index().await.unwrap();
        assert!(index.is_empty(), "Index should be empty when file doesn't exist");
    }

    #[tokio::test]
    async fn test_cannot_open_index_doc_directly() {
        let (_dir, store) = create_test_store();

        // Trying to open _index should fail
        let result = store.open_doc("_index").await;
        assert!(result.is_err(), "Should not be able to open _index directly");
    }

    #[tokio::test]
    async fn test_get_index_doc() {
        let (_dir, store) = create_test_store();

        // Create some documents to populate the index
        let _doc1 = store.open_doc("test/doc1").await.unwrap();
        let _doc2 = store.open_doc("test/doc2").await.unwrap();

        // Get the index document (for syncing)
        let index_doc = store.get_index_doc().await.unwrap();
        assert_eq!(index_doc.name(), "_index");
    }

    // =========================================================================
    // Tests for list_docs_with_metadata
    // =========================================================================

    #[tokio::test]
    async fn test_list_docs_with_metadata() {
        let (_dir, store) = create_test_store();

        // Create documents
        let _doc1 = store.open_doc("notes/a").await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        let _doc2 = store.open_doc("notes/b").await.unwrap();
        let _doc3 = store.open_doc("tasks/c").await.unwrap();

        // List with metadata
        let mut docs = store.list_docs_with_metadata("notes/").await.unwrap();
        docs.sort_by(|a, b| a.name.cmp(&b.name));

        assert_eq!(docs.len(), 2);
        assert_eq!(docs[0].name, "notes/a");
        assert_eq!(docs[1].name, "notes/b");

        // Both should have modification times
        assert!(docs[0].modified.is_some());
        assert!(docs[1].modified.is_some());
    }

    #[tokio::test]
    async fn test_list_docs_excludes_index() {
        let (_dir, store) = create_test_store();

        // Create a document (which creates the index)
        let _doc = store.open_doc("test/doc").await.unwrap();

        // List all docs
        let docs = store.list_docs("").unwrap();

        // Index should not be in the list
        assert!(!docs.contains(&"_index".to_string()), "Index should not appear in list_docs");
        assert_eq!(docs.len(), 1);
        assert!(docs.contains(&"test/doc".to_string()));
    }

    // =========================================================================
    // Tests for delete_doc
    // =========================================================================

    #[tokio::test]
    async fn test_delete_doc() {
        let (_dir, store) = create_test_store();

        // Create a document
        let doc = store.open_doc("test/to-delete").await.unwrap();
        doc.put("key", "value").unwrap();
        store.save_doc_atomic(&doc).await.unwrap();

        // Verify it exists
        assert!(doc.path().exists());
        let index = store.load_index().await.unwrap();
        assert!(index.contains_key("test/to-delete"));

        // Delete it
        store.delete_doc("test/to-delete").await.unwrap();

        // Verify it's gone
        assert!(!doc.path().exists(), "Document file should be deleted");
        let index = store.load_index().await.unwrap();
        assert!(!index.contains_key("test/to-delete"), "Document should be removed from index");
    }

    #[tokio::test]
    async fn test_delete_nonexistent_doc() {
        let (_dir, store) = create_test_store();

        // Deleting a non-existent document should succeed (idempotent)
        let result = store.delete_doc("nonexistent/doc").await;
        assert!(result.is_ok(), "Deleting non-existent doc should succeed");
    }

    #[tokio::test]
    async fn test_cannot_delete_index() {
        let (_dir, store) = create_test_store();

        // Create a doc to ensure index exists
        let _doc = store.open_doc("test/doc").await.unwrap();

        // Try to delete index
        let result = store.delete_doc("_index").await;
        assert!(result.is_err(), "Should not be able to delete _index");
    }

    #[tokio::test]
    async fn test_delete_cleans_up_temp_file() {
        let (_dir, store) = create_test_store();

        // Create a document
        let doc = store.open_doc("test/with-temp").await.unwrap();

        // Manually create a temp file to simulate interrupted save
        let temp_path = doc.path().with_extension("automerge.tmp");
        tokio::fs::write(&temp_path, b"temp data").await.unwrap();
        assert!(temp_path.exists());

        // Delete should clean up temp file too
        store.delete_doc("test/with-temp").await.unwrap();
        assert!(!temp_path.exists(), "Temp file should be cleaned up");
    }

    #[tokio::test]
    async fn test_delete_updates_list_docs() {
        let (_dir, store) = create_test_store();

        // Create multiple documents
        let _doc1 = store.open_doc("test/keep").await.unwrap();
        let _doc2 = store.open_doc("test/delete").await.unwrap();

        // Verify both exist
        let docs = store.list_docs("test/").unwrap();
        assert_eq!(docs.len(), 2);

        // Delete one
        store.delete_doc("test/delete").await.unwrap();

        // Verify only one remains
        let docs = store.list_docs("test/").unwrap();
        assert_eq!(docs.len(), 1);
        assert!(docs.contains(&"test/keep".to_string()));
        assert!(!docs.contains(&"test/delete".to_string()));
    }

    // =========================================================================
    // Tests for DocMetadata
    // =========================================================================

    #[test]
    fn test_doc_metadata_now() {
        let metadata = DocMetadata::now();
        assert!(!metadata.modified.is_empty());
        assert!(metadata.modified_time().is_some());
    }

    #[test]
    fn test_doc_metadata_invalid_timestamp() {
        let metadata = DocMetadata {
            modified: "invalid".to_string(),
        };
        assert!(metadata.modified_time().is_none());
    }
}
