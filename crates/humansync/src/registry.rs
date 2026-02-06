//! Device registry for tracking authorized devices
//!
//! This module provides:
//! - `DeviceInfo`: Information about a registered device
//! - `DeviceRegistry`: In-memory registry of authorized devices
//! - `DeviceRegistryStore`: Persistent registry backed by Automerge document
//!
//! The registry is synchronized across all devices via the `_devices` Automerge document.
//! Only devices in this registry are allowed to sync.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use automerge::{AutoCommit, ReadDoc, transaction::Transactable};
use chrono::{DateTime, Utc};
use iroh::NodeId;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use crate::error::{Error, Result};

/// The name of the devices document (special system document)
pub const DEVICES_DOC_NAME: &str = "_devices";

/// Information about a registered device
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceInfo {
    /// Device's Iroh NodeId
    pub node_id: NodeId,
    /// Human-readable device name
    pub name: String,
    /// When the device was paired
    pub paired_at: DateTime<Utc>,
    /// When the device was last seen
    pub last_seen: DateTime<Utc>,
}

impl DeviceInfo {
    /// Create a new device info
    pub fn new(node_id: NodeId, name: impl Into<String>) -> Self {
        let now = Utc::now();
        Self {
            node_id,
            name: name.into(),
            paired_at: now,
            last_seen: now,
        }
    }

    /// Update the last seen timestamp
    pub fn touch(&mut self) {
        self.last_seen = Utc::now();
    }
}

/// Registry of authorized devices (in-memory)
///
/// This is synchronized across all devices via the `_devices` Automerge document.
/// Only devices in this registry are allowed to sync.
pub struct DeviceRegistry {
    /// Map of NodeId to device info
    devices: RwLock<HashMap<NodeId, DeviceInfo>>,
}

impl DeviceRegistry {
    /// Create a new empty registry
    pub fn new() -> Self {
        Self {
            devices: RwLock::new(HashMap::new()),
        }
    }

    /// Add a device to the registry
    pub fn add(&self, info: DeviceInfo) {
        let mut devices = self.devices.write();
        devices.insert(info.node_id, info);
    }

    /// Remove a device from the registry
    pub fn remove(&self, node_id: &NodeId) -> Option<DeviceInfo> {
        let mut devices = self.devices.write();
        devices.remove(node_id)
    }

    /// Check if a device is registered
    pub fn contains(&self, node_id: &NodeId) -> bool {
        self.devices.read().contains_key(node_id)
    }

    /// Get device info
    pub fn get(&self, node_id: &NodeId) -> Option<DeviceInfo> {
        self.devices.read().get(node_id).cloned()
    }

    /// Get all registered devices
    pub fn list(&self) -> Vec<DeviceInfo> {
        self.devices.read().values().cloned().collect()
    }

    /// Get the number of registered devices
    pub fn len(&self) -> usize {
        self.devices.read().len()
    }

    /// Check if the registry is empty
    pub fn is_empty(&self) -> bool {
        self.devices.read().is_empty()
    }

    /// Update the last seen time for a device
    pub fn touch(&self, node_id: &NodeId) {
        let mut devices = self.devices.write();
        if let Some(info) = devices.get_mut(node_id) {
            info.touch();
        }
    }

    /// Load registry from device info list
    pub fn load_from_doc(&self, devices: Vec<DeviceInfo>) {
        let mut registry = self.devices.write();
        registry.clear();
        for info in devices {
            registry.insert(info.node_id, info);
        }
    }

    /// Export registry as device info list
    pub fn export(&self) -> Vec<DeviceInfo> {
        self.list()
    }

    /// Check if a NodeId is authorized for sync
    ///
    /// This is the connection gating check. Returns true if the node is
    /// in the device registry.
    pub fn is_authorized(&self, node_id: &NodeId) -> bool {
        self.contains(node_id)
    }
}

impl Default for DeviceRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Persistent device registry backed by an Automerge document
///
/// This wraps `DeviceRegistry` with Automerge-based persistence.
/// The registry is stored at `{storage_path}/docs/_devices.automerge`.
///
/// # Document Structure
///
/// The Automerge document has the following structure:
/// ```json
/// {
///   "devices": {
///     "{node_id}": {
///       "name": "Laptop",
///       "paired_at": "2026-02-05T10:00:00Z",
///       "last_seen": "2026-02-05T12:00:00Z"
///     }
///   }
/// }
/// ```
pub struct DeviceRegistryStore {
    /// In-memory registry for fast lookups
    registry: DeviceRegistry,
    /// Path to the Automerge document file
    doc_path: PathBuf,
    /// The Automerge document
    doc: RwLock<AutoCommit>,
    /// Whether there are unsaved changes
    dirty: RwLock<bool>,
}

impl DeviceRegistryStore {
    /// Create a new registry store, loading from disk if exists
    ///
    /// # Arguments
    /// * `storage_path` - Base storage path (e.g., `~/.humansync`)
    pub fn new(storage_path: &Path) -> Result<Self> {
        let docs_path = storage_path.join("docs");
        std::fs::create_dir_all(&docs_path)
            .map_err(|e| Error::storage(format!("failed to create docs directory: {e}")))?;

        let doc_path = docs_path.join(format!("{DEVICES_DOC_NAME}.automerge"));

        let (doc, devices) = if doc_path.exists() {
            // Load existing document
            let bytes = std::fs::read(&doc_path)
                .map_err(|e| Error::storage(format!("failed to read devices document: {e}")))?;
            let doc = AutoCommit::load(&bytes)
                .map_err(|e| Error::storage(format!("failed to load devices document: {e}")))?;
            let devices = Self::extract_devices_from_doc(&doc)?;
            info!(
                path = %doc_path.display(),
                device_count = devices.len(),
                "Loaded device registry from disk"
            );
            (doc, devices)
        } else {
            // Create new empty document - don't create "devices" map yet
            // The map will be created lazily on first add, or obtained via merge
            // This prevents conflict when two independent documents both create
            // the same map key at root
            let doc = AutoCommit::new();
            info!(path = %doc_path.display(), "Created new device registry");
            (doc, Vec::new())
        };

        let registry = DeviceRegistry::new();
        registry.load_from_doc(devices);

        Ok(Self {
            registry,
            doc_path,
            doc: RwLock::new(doc),
            dirty: RwLock::new(false),
        })
    }

    /// Get the path to the devices document
    #[must_use]
    pub fn doc_path(&self) -> &Path {
        &self.doc_path
    }

    /// Check if a NodeId is authorized for sync (connection gating)
    ///
    /// This is called before accepting sync connections.
    /// Unknown NodeIds are rejected.
    #[must_use]
    pub fn is_authorized(&self, node_id: &NodeId) -> bool {
        self.registry.is_authorized(node_id)
    }

    /// Add a device to the registry
    ///
    /// This updates both the in-memory registry and the Automerge document.
    /// Call `save()` to persist to disk.
    pub fn add(&self, info: DeviceInfo) -> Result<()> {
        let node_id_str = info.node_id.to_string();
        debug!(node_id = %node_id_str, name = %info.name, "Adding device to registry");

        // Update Automerge document
        {
            let mut doc = self.doc.write();

            // Get or create the devices map
            let devices_obj = match doc.get(automerge::ROOT, "devices")
                .map_err(|e| Error::storage(format!("failed to get devices map: {e}")))?
            {
                Some((_, obj_id)) => obj_id,
                None => {
                    doc.put_object(automerge::ROOT, "devices", automerge::ObjType::Map)
                        .map_err(|e| Error::storage(format!("failed to create devices map: {e}")))?
                }
            };

            // Create device entry as a nested map
            let device_obj = doc.put_object(&devices_obj, &node_id_str, automerge::ObjType::Map)
                .map_err(|e| Error::storage(format!("failed to create device entry: {e}")))?;

            // Set device properties
            doc.put(&device_obj, "name", info.name.clone())
                .map_err(|e| Error::storage(format!("failed to set device name: {e}")))?;
            doc.put(&device_obj, "paired_at", info.paired_at.to_rfc3339())
                .map_err(|e| Error::storage(format!("failed to set paired_at: {e}")))?;
            doc.put(&device_obj, "last_seen", info.last_seen.to_rfc3339())
                .map_err(|e| Error::storage(format!("failed to set last_seen: {e}")))?;
        }

        // Update in-memory registry
        self.registry.add(info);
        *self.dirty.write() = true;

        Ok(())
    }

    /// Remove a device from the registry
    ///
    /// This updates both the in-memory registry and the Automerge document.
    /// Call `save()` to persist to disk.
    pub fn remove(&self, node_id: &NodeId) -> Result<Option<DeviceInfo>> {
        let node_id_str = node_id.to_string();
        debug!(node_id = %node_id_str, "Removing device from registry");

        // Update Automerge document
        {
            let mut doc = self.doc.write();

            if let Some((_, devices_obj)) = doc.get(automerge::ROOT, "devices")
                .map_err(|e| Error::storage(format!("failed to get devices map: {e}")))?
            {
                doc.delete(&devices_obj, &node_id_str)
                    .map_err(|e| Error::storage(format!("failed to delete device entry: {e}")))?;
            }
        }

        // Update in-memory registry
        let removed = self.registry.remove(node_id);
        if removed.is_some() {
            *self.dirty.write() = true;
        }

        Ok(removed)
    }

    /// Update the last seen timestamp for a device
    pub fn touch(&self, node_id: &NodeId) -> Result<()> {
        let now = Utc::now();
        let node_id_str = node_id.to_string();

        // Update Automerge document
        {
            let mut doc = self.doc.write();

            if let Some((_, devices_obj)) = doc.get(automerge::ROOT, "devices")
                .map_err(|e| Error::storage(format!("failed to get devices map: {e}")))?
            {
                if let Some((_, device_obj)) = doc.get(&devices_obj, &node_id_str)
                    .map_err(|e| Error::storage(format!("failed to get device entry: {e}")))?
                {
                    doc.put(&device_obj, "last_seen", now.to_rfc3339())
                        .map_err(|e| Error::storage(format!("failed to update last_seen: {e}")))?;
                }
            }
        }

        // Update in-memory registry
        self.registry.touch(node_id);
        *self.dirty.write() = true;

        Ok(())
    }

    /// Check if a device is registered
    #[must_use]
    pub fn contains(&self, node_id: &NodeId) -> bool {
        self.registry.contains(node_id)
    }

    /// Get device info
    #[must_use]
    pub fn get(&self, node_id: &NodeId) -> Option<DeviceInfo> {
        self.registry.get(node_id)
    }

    /// Get all registered devices
    #[must_use]
    pub fn list(&self) -> Vec<DeviceInfo> {
        self.registry.list()
    }

    /// Get the number of registered devices
    #[must_use]
    pub fn len(&self) -> usize {
        self.registry.len()
    }

    /// Check if the registry is empty
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.registry.is_empty()
    }

    /// Check if there are unsaved changes
    #[must_use]
    pub fn is_dirty(&self) -> bool {
        *self.dirty.read()
    }

    /// Reload the registry from the on-disk `_devices.automerge` file.
    ///
    /// This is called after sync cycles to pick up changes that were
    /// merged into the `_devices` doc during normal doc sync.
    pub fn reload_from_disk(&self) -> Result<()> {
        if !self.doc_path.exists() {
            return Ok(());
        }

        let bytes = std::fs::read(&self.doc_path)
            .map_err(|e| Error::storage(format!("failed to read devices document: {e}")))?;

        match AutoCommit::load(&bytes) {
            Ok(loaded_doc) => {
                let devices = Self::extract_devices_from_doc(&loaded_doc)?;
                self.registry.load_from_doc(devices);
                *self.doc.write() = loaded_doc;
                *self.dirty.write() = false;
                debug!(
                    path = %self.doc_path.display(),
                    count = self.registry.len(),
                    "Reloaded device registry from disk"
                );
            }
            Err(e) => {
                warn!(error = %e, "Failed to load _devices document, skipping reload");
            }
        }

        Ok(())
    }

    /// Save the registry to disk
    pub fn save(&self) -> Result<()> {
        let mut doc = self.doc.write();
        let bytes = doc.save();

        // Atomic write: write to temp file, then rename
        let temp_path = self.doc_path.with_extension("automerge.tmp");
        std::fs::write(&temp_path, bytes)
            .map_err(|e| Error::storage(format!("failed to write devices document: {e}")))?;
        std::fs::rename(&temp_path, &self.doc_path)
            .map_err(|e| Error::storage(format!("failed to rename devices document: {e}")))?;

        *self.dirty.write() = false;
        debug!(path = %self.doc_path.display(), "Saved device registry to disk");

        Ok(())
    }

    /// Get the raw Automerge document bytes for syncing
    pub fn save_bytes(&self) -> Vec<u8> {
        self.doc.write().save()
    }

    /// Merge changes from a remote Automerge document
    ///
    /// This is used during sync to incorporate changes from other peers.
    /// After merging, the in-memory registry is updated to reflect the new state.
    pub fn merge(&self, remote_bytes: &[u8]) -> Result<()> {
        let mut remote_doc = AutoCommit::load(remote_bytes)
            .map_err(|e| Error::sync(format!("failed to load remote devices document: {e}")))?;

        // Merge the remote document into our local document
        {
            let mut doc = self.doc.write();
            doc.merge(&mut remote_doc)
                .map_err(|e| Error::sync(format!("failed to merge devices documents: {e}")))?;
        }

        // Update in-memory registry from merged document
        let devices = Self::extract_devices_from_doc(&self.doc.read())?;
        self.registry.load_from_doc(devices);
        *self.dirty.write() = true;

        info!("Merged remote device registry changes");
        Ok(())
    }

    /// Generate a sync message for the given sync state
    pub fn generate_sync_message(
        &self,
        sync_state: &mut automerge::sync::State,
    ) -> Option<automerge::sync::Message> {
        use automerge::sync::SyncDoc;
        let mut doc = self.doc.write();
        doc.sync().generate_sync_message(sync_state)
    }

    /// Receive a sync message and update the document
    pub fn receive_sync_message(
        &self,
        sync_state: &mut automerge::sync::State,
        message: automerge::sync::Message,
    ) -> Result<()> {
        use automerge::sync::SyncDoc;

        {
            let mut doc = self.doc.write();
            doc.sync()
                .receive_sync_message(sync_state, message)
                .map_err(|e| Error::sync(format!("failed to receive sync message: {e}")))?;
        }

        // Update in-memory registry after receiving changes
        let devices = Self::extract_devices_from_doc(&self.doc.read())?;
        self.registry.load_from_doc(devices);
        *self.dirty.write() = true;

        Ok(())
    }

    /// Create a new sync state for synchronizing with a peer
    #[must_use]
    pub fn new_sync_state(&self) -> automerge::sync::State {
        automerge::sync::State::new()
    }

    /// Get a reference to the underlying in-memory registry
    #[must_use]
    pub fn registry(&self) -> &DeviceRegistry {
        &self.registry
    }

    /// Extract device info from an Automerge document
    fn extract_devices_from_doc(doc: &AutoCommit) -> Result<Vec<DeviceInfo>> {
        let mut devices = Vec::new();

        // Get the devices map
        let devices_obj = match doc.get(automerge::ROOT, "devices")
            .map_err(|e| Error::storage(format!("failed to get devices map: {e}")))?
        {
            Some((_, obj_id)) => obj_id,
            None => return Ok(devices), // No devices map yet
        };

        // Iterate over device entries
        for key in doc.keys(&devices_obj) {
            // Parse node_id from key
            let node_id: NodeId = match key.parse() {
                Ok(id) => id,
                Err(e) => {
                    warn!(key = %key, error = %e, "Failed to parse node_id, skipping device");
                    continue;
                }
            };

            // Get device object
            let device_obj = match doc.get(&devices_obj, &key)
                .map_err(|e| Error::storage(format!("failed to get device {key}: {e}")))?
            {
                Some((_, obj_id)) => obj_id,
                None => continue,
            };

            // Extract device properties
            let name = Self::get_string_field(doc, &device_obj, "name")
                .unwrap_or_else(|| "Unknown".to_string());

            let paired_at = Self::get_datetime_field(doc, &device_obj, "paired_at")
                .unwrap_or_else(Utc::now);

            let last_seen = Self::get_datetime_field(doc, &device_obj, "last_seen")
                .unwrap_or_else(Utc::now);

            devices.push(DeviceInfo {
                node_id,
                name,
                paired_at,
                last_seen,
            });
        }

        Ok(devices)
    }

    /// Helper to extract a string field from an Automerge object
    fn get_string_field(doc: &AutoCommit, obj: &automerge::ObjId, key: &str) -> Option<String> {
        match doc.get(obj, key) {
            Ok(Some((automerge::Value::Scalar(s), _))) => {
                if let automerge::ScalarValue::Str(s) = s.as_ref() {
                    Some(s.to_string())
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    /// Helper to extract a datetime field from an Automerge object
    fn get_datetime_field(doc: &AutoCommit, obj: &automerge::ObjId, key: &str) -> Option<DateTime<Utc>> {
        Self::get_string_field(doc, obj, key)
            .and_then(|s| DateTime::parse_from_rfc3339(&s).ok())
            .map(|dt| dt.with_timezone(&Utc))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iroh::SecretKey;
    use tempfile::TempDir;

    fn test_node_id() -> NodeId {
        // Generate a valid NodeId from a deterministic secret key
        let key_bytes = [1u8; 32];
        let secret_key = SecretKey::from_bytes(&key_bytes);
        secret_key.public()
    }

    fn test_node_id_2() -> NodeId {
        // Generate a different valid NodeId
        let key_bytes = [2u8; 32];
        let secret_key = SecretKey::from_bytes(&key_bytes);
        secret_key.public()
    }

    fn test_node_id_3() -> NodeId {
        // Generate another different valid NodeId
        let key_bytes = [3u8; 32];
        let secret_key = SecretKey::from_bytes(&key_bytes);
        secret_key.public()
    }

    // ========================================
    // DeviceRegistry (in-memory) tests
    // ========================================

    #[test]
    fn test_registry_add_and_contains() {
        let registry = DeviceRegistry::new();
        let node_id = test_node_id();

        assert!(!registry.contains(&node_id));

        let info = DeviceInfo::new(node_id, "Test Device");
        registry.add(info);

        assert!(registry.contains(&node_id));
    }

    #[test]
    fn test_registry_remove() {
        let registry = DeviceRegistry::new();
        let node_id = test_node_id();

        let info = DeviceInfo::new(node_id, "Test Device");
        registry.add(info);
        assert!(registry.contains(&node_id));

        let removed = registry.remove(&node_id);
        assert!(removed.is_some());
        assert!(!registry.contains(&node_id));
    }

    #[test]
    fn test_registry_list() {
        let registry = DeviceRegistry::new();

        registry.add(DeviceInfo::new(test_node_id(), "Device 1"));
        registry.add(DeviceInfo::new(test_node_id_2(), "Device 2"));

        let devices = registry.list();
        assert_eq!(devices.len(), 2);
    }

    #[test]
    fn test_registry_touch() {
        let registry = DeviceRegistry::new();
        let node_id = test_node_id();

        let info = DeviceInfo::new(node_id, "Test Device");
        let original_last_seen = info.last_seen;
        registry.add(info);

        std::thread::sleep(std::time::Duration::from_millis(10));
        registry.touch(&node_id);

        let updated = registry.get(&node_id).unwrap();
        assert!(updated.last_seen > original_last_seen);
    }

    #[test]
    fn test_registry_load_export() {
        let registry = DeviceRegistry::new();

        registry.add(DeviceInfo::new(test_node_id(), "Device 1"));
        registry.add(DeviceInfo::new(test_node_id_2(), "Device 2"));

        let exported = registry.export();
        assert_eq!(exported.len(), 2);

        let new_registry = DeviceRegistry::new();
        new_registry.load_from_doc(exported);
        assert_eq!(new_registry.len(), 2);
    }

    #[test]
    fn test_registry_is_authorized() {
        let registry = DeviceRegistry::new();
        let node_id = test_node_id();
        let unknown_id = test_node_id_2();

        // Unknown node should not be authorized
        assert!(!registry.is_authorized(&node_id));
        assert!(!registry.is_authorized(&unknown_id));

        // Add a device
        registry.add(DeviceInfo::new(node_id, "Test Device"));

        // Known node should be authorized, unknown should not
        assert!(registry.is_authorized(&node_id));
        assert!(!registry.is_authorized(&unknown_id));
    }

    // ========================================
    // DeviceRegistryStore (persistent) tests
    // ========================================

    fn create_test_store() -> (TempDir, DeviceRegistryStore) {
        let temp_dir = TempDir::new().unwrap();
        let store = DeviceRegistryStore::new(temp_dir.path()).unwrap();
        (temp_dir, store)
    }

    #[test]
    fn test_store_new_creates_doc() {
        let (temp_dir, store) = create_test_store();

        // Should be empty initially
        assert!(store.is_empty());
        assert_eq!(store.len(), 0);

        // Doc path should be set correctly
        let expected_path = temp_dir.path().join("docs").join("_devices.automerge");
        assert_eq!(store.doc_path(), expected_path);
    }

    #[test]
    fn test_store_add_device() {
        let (_dir, store) = create_test_store();
        let node_id = test_node_id();

        let info = DeviceInfo::new(node_id, "Test Laptop");
        store.add(info).unwrap();

        assert!(store.contains(&node_id));
        assert_eq!(store.len(), 1);

        let retrieved = store.get(&node_id).unwrap();
        assert_eq!(retrieved.name, "Test Laptop");
    }

    #[test]
    fn test_store_remove_device() {
        let (_dir, store) = create_test_store();
        let node_id = test_node_id();

        store.add(DeviceInfo::new(node_id, "Test Device")).unwrap();
        assert!(store.contains(&node_id));

        let removed = store.remove(&node_id).unwrap();
        assert!(removed.is_some());
        assert!(!store.contains(&node_id));
    }

    #[test]
    fn test_store_is_authorized() {
        let (_dir, store) = create_test_store();
        let known_id = test_node_id();
        let unknown_id = test_node_id_2();

        // Initially no one is authorized
        assert!(!store.is_authorized(&known_id));
        assert!(!store.is_authorized(&unknown_id));

        // Add a device
        store.add(DeviceInfo::new(known_id, "Known Device")).unwrap();

        // Known device should be authorized
        assert!(store.is_authorized(&known_id));
        // Unknown device should not be authorized
        assert!(!store.is_authorized(&unknown_id));
    }

    #[test]
    fn test_store_persistence_save_and_load() {
        let temp_dir = TempDir::new().unwrap();
        let node_id = test_node_id();

        // Create store, add device, save
        {
            let store = DeviceRegistryStore::new(temp_dir.path()).unwrap();
            store.add(DeviceInfo::new(node_id, "Persistent Device")).unwrap();
            store.save().unwrap();
        }

        // Create new store from same path, should load saved data
        {
            let store = DeviceRegistryStore::new(temp_dir.path()).unwrap();
            assert_eq!(store.len(), 1);
            assert!(store.contains(&node_id));

            let device = store.get(&node_id).unwrap();
            assert_eq!(device.name, "Persistent Device");
        }
    }

    #[test]
    fn test_store_persistence_multiple_devices() {
        let temp_dir = TempDir::new().unwrap();
        let node_id_1 = test_node_id();
        let node_id_2 = test_node_id_2();

        // Create and populate store
        {
            let store = DeviceRegistryStore::new(temp_dir.path()).unwrap();
            store.add(DeviceInfo::new(node_id_1, "Laptop")).unwrap();
            store.add(DeviceInfo::new(node_id_2, "Phone")).unwrap();
            store.save().unwrap();
        }

        // Reload and verify
        {
            let store = DeviceRegistryStore::new(temp_dir.path()).unwrap();
            assert_eq!(store.len(), 2);
            assert!(store.contains(&node_id_1));
            assert!(store.contains(&node_id_2));

            let device1 = store.get(&node_id_1).unwrap();
            let device2 = store.get(&node_id_2).unwrap();
            assert_eq!(device1.name, "Laptop");
            assert_eq!(device2.name, "Phone");
        }
    }

    #[test]
    fn test_store_dirty_flag() {
        let (_dir, store) = create_test_store();
        let node_id = test_node_id();

        // Initially not dirty
        assert!(!store.is_dirty());

        // Add device makes it dirty
        store.add(DeviceInfo::new(node_id, "Test")).unwrap();
        assert!(store.is_dirty());

        // Save clears dirty flag
        store.save().unwrap();
        assert!(!store.is_dirty());
    }

    #[test]
    fn test_store_touch() {
        let (_dir, store) = create_test_store();
        let node_id = test_node_id();

        store.add(DeviceInfo::new(node_id, "Test")).unwrap();
        let original = store.get(&node_id).unwrap();

        std::thread::sleep(std::time::Duration::from_millis(10));
        store.touch(&node_id).unwrap();

        let updated = store.get(&node_id).unwrap();
        assert!(updated.last_seen > original.last_seen);
    }

    #[test]
    fn test_store_list_devices() {
        let (_dir, store) = create_test_store();

        store.add(DeviceInfo::new(test_node_id(), "Device 1")).unwrap();
        store.add(DeviceInfo::new(test_node_id_2(), "Device 2")).unwrap();

        let devices = store.list();
        assert_eq!(devices.len(), 2);

        let names: Vec<_> = devices.iter().map(|d| d.name.as_str()).collect();
        assert!(names.contains(&"Device 1"));
        assert!(names.contains(&"Device 2"));
    }

    #[test]
    fn test_store_sequential_sync() {
        // Test the basic sync flow: server creates registry, client syncs,
        // client adds device, server syncs back.
        // This is the realistic flow - devices always pair through the server.
        let temp_dir_1 = TempDir::new().unwrap();
        let temp_dir_2 = TempDir::new().unwrap();

        let node_id_1 = test_node_id();
        let node_id_2 = test_node_id_2();

        // Server creates initial registry with device A (the server's own device)
        let server = DeviceRegistryStore::new(temp_dir_1.path()).unwrap();
        server.add(DeviceInfo::new(node_id_1, "Server")).unwrap();
        assert_eq!(server.len(), 1);

        // Client pairs: first sync from server to get the registry
        let client = DeviceRegistryStore::new(temp_dir_2.path()).unwrap();
        client.merge(&server.save_bytes()).unwrap();
        assert_eq!(client.len(), 1);
        assert!(client.contains(&node_id_1));

        // Server adds the client device during pairing
        server.add(DeviceInfo::new(node_id_2, "Client")).unwrap();
        assert_eq!(server.len(), 2);

        // Client syncs again to get the updated registry
        client.merge(&server.save_bytes()).unwrap();
        assert_eq!(client.len(), 2);
        assert!(client.contains(&node_id_1));
        assert!(client.contains(&node_id_2));
    }

    #[test]
    fn test_store_sync_protocol_bidirectional() {
        // Test that the Automerge sync protocol properly exchanges changes
        // between two stores that started from a common state.
        let temp_dir_1 = TempDir::new().unwrap();
        let temp_dir_2 = TempDir::new().unwrap();

        let node_id_1 = test_node_id();
        let node_id_2 = test_node_id_2();

        // Server creates registry with both devices (pairing complete)
        let server = DeviceRegistryStore::new(temp_dir_1.path()).unwrap();
        server.add(DeviceInfo::new(node_id_1, "Server")).unwrap();
        server.add(DeviceInfo::new(node_id_2, "Client")).unwrap();

        // Client syncs from server
        let client = DeviceRegistryStore::new(temp_dir_2.path()).unwrap();
        client.merge(&server.save_bytes()).unwrap();
        assert_eq!(client.len(), 2);

        // Server updates last_seen for a device
        server.touch(&node_id_1).unwrap();

        // Use the sync protocol to propagate changes
        let mut server_sync = server.new_sync_state();
        let mut client_sync = client.new_sync_state();

        for _round in 0..10 {
            let msg1 = server.generate_sync_message(&mut server_sync);
            let msg2 = client.generate_sync_message(&mut client_sync);

            if msg1.is_none() && msg2.is_none() {
                break;
            }

            if let Some(msg) = msg1 {
                client.receive_sync_message(&mut client_sync, msg).unwrap();
            }
            if let Some(msg) = msg2 {
                server.receive_sync_message(&mut server_sync, msg).unwrap();
            }
        }

        // Both should still have both devices
        assert_eq!(server.len(), 2);
        assert_eq!(client.len(), 2);
    }

    #[test]
    fn test_store_revocation_propagates() {
        // Test that when a device is revoked on the server,
        // the revocation propagates to all clients.
        let temp_dir_server = TempDir::new().unwrap();
        let temp_dir_client = TempDir::new().unwrap();

        let server_id = test_node_id();
        let client_id = test_node_id_2();
        let revoked_id = test_node_id_3();

        // Server creates registry with all devices
        let server = DeviceRegistryStore::new(temp_dir_server.path()).unwrap();
        server.add(DeviceInfo::new(server_id, "Server")).unwrap();
        server.add(DeviceInfo::new(client_id, "Client")).unwrap();
        server.add(DeviceInfo::new(revoked_id, "ToRevoke")).unwrap();

        // Client syncs from server
        let client = DeviceRegistryStore::new(temp_dir_client.path()).unwrap();
        client.merge(&server.save_bytes()).unwrap();
        assert_eq!(client.len(), 3);
        assert!(client.is_authorized(&revoked_id));

        // Server revokes a device
        server.remove(&revoked_id).unwrap();
        assert!(!server.is_authorized(&revoked_id));
        assert_eq!(server.len(), 2);

        // Client syncs again - revocation should propagate
        client.merge(&server.save_bytes()).unwrap();
        assert!(!client.is_authorized(&revoked_id));
        assert_eq!(client.len(), 2);

        // Other devices should still be authorized
        assert!(client.is_authorized(&server_id));
        assert!(client.is_authorized(&client_id));
    }

    #[test]
    fn test_store_offline_edits_merge() {
        // Test that edits made while offline can be merged when reconnecting.
        // Both devices have a shared history, then make independent changes offline.
        let temp_dir_1 = TempDir::new().unwrap();
        let temp_dir_2 = TempDir::new().unwrap();

        let device_a = test_node_id();
        let device_b = test_node_id_2();
        let device_c = test_node_id_3();

        // Initial state: store1 has device A
        let store1 = DeviceRegistryStore::new(temp_dir_1.path()).unwrap();
        store1.add(DeviceInfo::new(device_a, "Device A")).unwrap();

        // Store2 syncs to get device A
        let store2 = DeviceRegistryStore::new(temp_dir_2.path()).unwrap();
        store2.merge(&store1.save_bytes()).unwrap();
        assert_eq!(store2.len(), 1);

        // Both go offline and make changes
        store1.add(DeviceInfo::new(device_b, "Device B")).unwrap();
        store2.add(DeviceInfo::new(device_c, "Device C")).unwrap();

        // Store2 has 2 devices (A and C)
        assert_eq!(store2.len(), 2);
        // Store1 has 2 devices (A and B)
        assert_eq!(store1.len(), 2);

        // Now they reconnect and sync
        // Store1 gets store2's changes
        store1.merge(&store2.save_bytes()).unwrap();
        // Store2 gets store1's changes
        store2.merge(&store1.save_bytes()).unwrap();

        // Both should have all 3 devices
        assert_eq!(store1.len(), 3);
        assert_eq!(store2.len(), 3);
        assert!(store1.contains(&device_a));
        assert!(store1.contains(&device_b));
        assert!(store1.contains(&device_c));
        assert!(store2.contains(&device_a));
        assert!(store2.contains(&device_b));
        assert!(store2.contains(&device_c));
    }
}
