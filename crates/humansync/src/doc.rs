//! Document handle for reading and writing data.
//!
//! This module provides the [`Document`] struct for working with Automerge CRDT
//! documents. Documents support JSON-like data with automatic conflict resolution.
//!
//! # Features
//!
//! - **Type-safe access**: Put and get values with Rust types (via serde)
//! - **Conflict-free**: Concurrent edits on different devices merge automatically
//! - **Instant reads/writes**: All operations are local, no network round-trips
//! - **Persistence**: Documents are saved to disk and survive restarts
//!
//! # Example
//!
//! ```rust,no_run
//! use humansync::{HumanSync, Config};
//!
//! # async fn example() -> humansync::Result<()> {
//! let node = HumanSync::init(Config::default()).await?;
//!
//! // Open or create a document
//! let doc = node.open_doc("notes/shopping").await?;
//!
//! // Write values (any type that implements Serialize)
//! doc.put("eggs", 12)?;
//! doc.put("milk", true)?;
//! doc.put("store", "Costco")?;
//!
//! // Read values (any type that implements Deserialize)
//! let eggs: Option<i64> = doc.get("eggs")?;
//! let store: Option<String> = doc.get("store")?;
//!
//! // List keys
//! let keys = doc.keys()?;
//! println!("Keys: {:?}", keys);
//!
//! // Save changes to disk
//! doc.save()?;
//! # Ok(())
//! # }
//! ```

use std::path::PathBuf;

use automerge::{AutoCommit, ObjType, ReadDoc, transaction::Transactable};
use parking_lot::RwLock;
use serde::{de::DeserializeOwned, Serialize};

use crate::error::{Error, Result};

/// Cursor position info for a remote device.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CursorInfo {
    /// Short device identifier.
    pub device_id: String,
    /// Human-readable device name.
    pub device_name: String,
    /// Cursor position (character offset).
    pub position: u64,
    /// CSS color string for this device.
    pub color: String,
    /// Device activity: "typing" or "viewing"
    pub activity: String,
}

/// A handle to an Automerge document.
///
/// Documents support JSON-like data with automatic conflict resolution via CRDTs.
/// All reads and writes are local and instant - changes sync in the background.
///
/// # Supported Types
///
/// The `put` and `get` methods work with any type that implements serde's
/// `Serialize` and `Deserialize` traits. Primitive types map directly:
///
/// - `String`, `&str` -> Automerge string
/// - `i64`, `u64`, `f64` -> Automerge numbers
/// - `bool` -> Automerge boolean
/// - `()` or `None` -> Automerge null
///
/// Complex types (arrays, objects) are serialized as JSON strings.
///
/// # Persistence
///
/// Documents are automatically saved when modified through sync operations.
/// You can also manually save with [`Document::save()`]. The `is_dirty()` method
/// indicates whether there are unsaved changes.
///
/// # Thread Safety
///
/// Document operations use internal locking and can be called from multiple
/// async tasks concurrently.
pub struct Document {
    /// Document name/path
    name: String,
    /// Path on disk
    path: PathBuf,
    /// The Automerge document
    doc: RwLock<AutoCommit>,
    /// Whether the document has unsaved changes
    dirty: RwLock<bool>,
}

impl Document {
    /// Create a new document handle
    ///
    /// This is primarily for internal use. Applications should use
    /// `HumanSync::open_doc()` instead.
    #[doc(hidden)]
    pub fn new(name: String, path: PathBuf, doc: AutoCommit) -> Self {
        Self {
            name,
            path,
            doc: RwLock::new(doc),
            dirty: RwLock::new(false),
        }
    }

    /// Get the document name
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Get the document path on disk
    #[must_use]
    pub fn path(&self) -> &PathBuf {
        &self.path
    }

    /// Put a value at the given key
    ///
    /// The value must be serializable to JSON.
    pub fn put<V: Serialize>(&self, key: &str, value: V) -> Result<()> {
        let json_value = serde_json::to_value(&value)
            .map_err(|e| Error::document(format!("failed to serialize value: {e}")))?;

        let mut doc = self.doc.write();

        // Convert JSON value to Automerge scalar
        let scalar = json_to_automerge_value(&json_value)?;
        doc.put(automerge::ROOT, key, scalar)
            .map_err(|e| Error::document(format!("failed to put value: {e}")))?;

        *self.dirty.write() = true;
        Ok(())
    }

    /// Get a value at the given key
    ///
    /// Returns `None` if the key doesn't exist.
    pub fn get<V: DeserializeOwned>(&self, key: &str) -> Result<Option<V>> {
        let doc = self.doc.read();

        let value = doc
            .get(automerge::ROOT, key)
            .map_err(|e| Error::document(format!("failed to get value: {e}")))?;

        match value {
            Some((value, _)) => {
                let json_value = automerge_value_to_json(&value)?;
                let typed_value = serde_json::from_value(json_value)
                    .map_err(|e| Error::document(format!("failed to deserialize value: {e}")))?;
                Ok(Some(typed_value))
            }
            None => Ok(None),
        }
    }

    /// Delete a key from the document
    pub fn delete(&self, key: &str) -> Result<()> {
        let mut doc = self.doc.write();
        doc.delete(automerge::ROOT, key)
            .map_err(|e| Error::document(format!("failed to delete key: {e}")))?;
        *self.dirty.write() = true;
        Ok(())
    }

    /// Check if a key exists
    pub fn contains(&self, key: &str) -> Result<bool> {
        let doc = self.doc.read();
        let value = doc
            .get(automerge::ROOT, key)
            .map_err(|e| Error::document(format!("failed to check key: {e}")))?;
        Ok(value.is_some())
    }

    /// Get all keys in the document root
    pub fn keys(&self) -> Result<Vec<String>> {
        let doc = self.doc.read();
        let keys = doc
            .keys(automerge::ROOT)
            .map(|k| k.to_string())
            .collect();
        Ok(keys)
    }

    /// Check if the document has unsaved changes
    #[must_use]
    pub fn is_dirty(&self) -> bool {
        *self.dirty.read()
    }

    /// Save the document to disk
    pub fn save(&self) -> Result<()> {
        let mut doc = self.doc.write();
        let bytes = doc.save();

        std::fs::write(&self.path, bytes)
            .map_err(|e| Error::storage(format!("failed to save document: {e}")))?;

        *self.dirty.write() = false;
        Ok(())
    }

    /// Read content as a Text object, with scalar fallback.
    pub fn get_text(&self, key: &str) -> Result<Option<String>> {
        let doc = self.doc.read();
        match doc.get(automerge::ROOT, key) {
            Ok(Some((automerge::Value::Object(ObjType::Text), obj_id))) => {
                doc.text(&obj_id)
                    .map(Some)
                    .map_err(|e| Error::document(format!("failed to read text: {e}")))
            }
            Ok(Some((automerge::Value::Scalar(s), _))) => {
                match s.as_ref() {
                    automerge::ScalarValue::Str(s) => Ok(Some(s.to_string())),
                    _ => Ok(None),
                }
            }
            Ok(None) => Ok(None),
            Ok(Some(_)) => Ok(None),
            Err(e) => Err(Error::document(format!("failed to get text: {e}"))),
        }
    }

    /// Ensure the given key holds an Automerge Text object, migrating from scalar if needed.
    pub fn ensure_text(&self, key: &str) -> Result<()> {
        let mut doc = self.doc.write();
        match doc.get(automerge::ROOT, key) {
            Ok(Some((automerge::Value::Object(ObjType::Text), _))) => Ok(()),
            Ok(Some((automerge::Value::Scalar(s), _))) => {
                let old = match s.as_ref() {
                    automerge::ScalarValue::Str(s) => s.to_string(),
                    _ => String::new(),
                };
                doc.delete(automerge::ROOT, key)
                    .map_err(|e| Error::document(format!("failed to delete scalar: {e}")))?;
                let obj_id = doc.put_object(automerge::ROOT, key, ObjType::Text)
                    .map_err(|e| Error::document(format!("failed to create text object: {e}")))?;
                if !old.is_empty() {
                    doc.splice_text(&obj_id, 0, 0, &old)
                        .map_err(|e| Error::document(format!("failed to splice migrated text: {e}")))?;
                }
                *self.dirty.write() = true;
                Ok(())
            }
            Ok(None) => {
                doc.put_object(automerge::ROOT, key, ObjType::Text)
                    .map_err(|e| Error::document(format!("failed to create text object: {e}")))?;
                *self.dirty.write() = true;
                Ok(())
            }
            Ok(Some(_)) => Err(Error::document("unexpected value type for text key")),
            Err(e) => Err(Error::document(format!("failed to check text key: {e}"))),
        }
    }

    /// Update a Text object by computing a minimal diff and splicing.
    pub fn update_text(&self, key: &str, new_content: &str) -> Result<()> {
        // Ensure it's a Text object first
        self.ensure_text(key)?;

        let mut doc = self.doc.write();
        let obj_id = match doc.get(automerge::ROOT, key) {
            Ok(Some((automerge::Value::Object(ObjType::Text), obj_id))) => obj_id,
            _ => return Err(Error::document("content is not a Text object after ensure")),
        };

        let old = doc.text(&obj_id)
            .map_err(|e| Error::document(format!("failed to read current text: {e}")))?;

        if old == new_content {
            return Ok(());
        }

        // Compute minimal diff: common prefix and suffix
        let old_bytes = old.as_bytes();
        let new_bytes = new_content.as_bytes();

        let prefix = old_bytes.iter().zip(new_bytes.iter())
            .take_while(|(a, b)| a == b)
            .count();

        let suffix = old_bytes[prefix..].iter().rev()
            .zip(new_bytes[prefix..].iter().rev())
            .take_while(|(a, b)| a == b)
            .count();

        // Ensure we're at character boundaries
        let prefix = floor_char_boundary(&old, prefix);
        let suffix_old = floor_char_boundary_rev(&old, old.len() - suffix);
        let suffix_new = floor_char_boundary_rev(new_content, new_content.len() - suffix);

        let del_count = old[prefix..suffix_old].chars().count();
        let insert = &new_content[prefix..suffix_new];

        // Convert byte prefix to char position
        let char_pos = old[..prefix].chars().count();

        doc.splice_text(&obj_id, char_pos, del_count as isize, insert)
            .map_err(|e| Error::document(format!("failed to splice text: {e}")))?;

        *self.dirty.write() = true;
        Ok(())
    }

    /// Set cursor position for a device.
    pub fn set_cursor(&self, device_id: &str, position: u64, device_name: &str, activity: &str) -> Result<()> {
        let colors = ["#FF3B30", "#007AFF", "#34C759", "#FF9500", "#AF52DE", "#FF2D55"];
        let hash: usize = device_id.bytes().map(|b| b as usize).sum();
        let color = colors[hash % colors.len()].to_string();

        let info = CursorInfo {
            device_id: device_id.to_string(),
            device_name: device_name.to_string(),
            position,
            color,
            activity: activity.to_string(),
        };
        let json = serde_json::to_string(&info)
            .map_err(|e| Error::document(format!("failed to serialize cursor: {e}")))?;

        let key = format!("cursor_{device_id}");
        let mut doc = self.doc.write();
        doc.put(automerge::ROOT, &key, json.as_str())
            .map_err(|e| Error::document(format!("failed to set cursor: {e}")))?;
        *self.dirty.write() = true;
        Ok(())
    }

    /// Get cursor positions for all devices except the one specified.
    pub fn get_cursors(&self, exclude_device: &str) -> Result<Vec<CursorInfo>> {
        let doc = self.doc.read();
        let exclude_key = format!("cursor_{exclude_device}");
        let mut cursors = Vec::new();

        for key in doc.keys(automerge::ROOT) {
            if !key.starts_with("cursor_") || key == exclude_key {
                continue;
            }
            if let Ok(Some((automerge::Value::Scalar(s), _))) = doc.get(automerge::ROOT, &key) {
                if let automerge::ScalarValue::Str(json_str) = s.as_ref() {
                    if let Ok(info) = serde_json::from_str::<CursorInfo>(json_str) {
                        cursors.push(info);
                    }
                }
            }
        }

        Ok(cursors)
    }

    /// Get the raw Automerge document bytes for syncing
    pub(crate) fn save_bytes(&self) -> Vec<u8> {
        self.doc.write().save()
    }

    /// Generate a sync message for the given sync state
    ///
    /// Returns `None` if there are no changes to send.
    pub(crate) fn generate_sync_message(
        &self,
        sync_state: &mut automerge::sync::State,
    ) -> Option<automerge::sync::Message> {
        use automerge::sync::SyncDoc;
        let mut doc = self.doc.write();
        doc.sync().generate_sync_message(sync_state)
    }

    /// Receive a sync message and update the document
    ///
    /// Returns `Ok(())` if the message was applied successfully.
    pub(crate) fn receive_sync_message(
        &self,
        sync_state: &mut automerge::sync::State,
        message: automerge::sync::Message,
    ) -> Result<()> {
        use automerge::sync::SyncDoc;
        let mut doc = self.doc.write();
        doc.sync()
            .receive_sync_message(sync_state, message)
            .map_err(|e| Error::sync(format!("failed to receive sync message: {e}")))?;
        *self.dirty.write() = true;
        Ok(())
    }

    /// Create a new sync state for synchronizing with a peer
    pub(crate) fn new_sync_state(&self) -> automerge::sync::State {
        automerge::sync::State::new()
    }

    /// Get the heads (version vector) of this document
    ///
    /// This is useful for verifying that two documents are in sync.
    #[doc(hidden)]
    pub fn get_heads(&self) -> Vec<automerge::ChangeHash> {
        let mut doc = self.doc.write();
        doc.get_heads()
    }
}

/// Find the largest byte index <= pos that is a char boundary
fn floor_char_boundary(s: &str, pos: usize) -> usize {
    if pos >= s.len() {
        s.len()
    } else {
        let mut p = pos;
        while p > 0 && !s.is_char_boundary(p) {
            p -= 1;
        }
        p
    }
}

/// Find the smallest byte index >= pos that is a char boundary
fn floor_char_boundary_rev(s: &str, pos: usize) -> usize {
    if pos >= s.len() {
        s.len()
    } else {
        let mut p = pos;
        while p < s.len() && !s.is_char_boundary(p) {
            p += 1;
        }
        p
    }
}

/// Convert a JSON value to an Automerge scalar value
fn json_to_automerge_value(value: &serde_json::Value) -> Result<automerge::ScalarValue> {
    use automerge::ScalarValue;
    use serde_json::Value;

    match value {
        Value::Null => Ok(ScalarValue::Null),
        Value::Bool(b) => Ok(ScalarValue::Boolean(*b)),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(ScalarValue::Int(i))
            } else if let Some(u) = n.as_u64() {
                Ok(ScalarValue::Uint(u))
            } else if let Some(f) = n.as_f64() {
                Ok(ScalarValue::F64(f))
            } else {
                Err(Error::document("unsupported number type"))
            }
        }
        Value::String(s) => Ok(ScalarValue::Str(s.clone().into())),
        Value::Array(_) | Value::Object(_) => {
            // For complex types, store as JSON string
            let json_str = serde_json::to_string(value)
                .map_err(|e| Error::document(format!("failed to serialize complex value: {e}")))?;
            Ok(ScalarValue::Str(json_str.into()))
        }
    }
}

/// Convert an Automerge value to JSON
fn automerge_value_to_json(value: &automerge::Value) -> Result<serde_json::Value> {
    use automerge::Value;
    use serde_json::Value as JsonValue;

    match value {
        Value::Scalar(s) => match s.as_ref() {
            automerge::ScalarValue::Null => Ok(JsonValue::Null),
            automerge::ScalarValue::Boolean(b) => Ok(JsonValue::Bool(*b)),
            automerge::ScalarValue::Int(i) => Ok(JsonValue::Number((*i).into())),
            automerge::ScalarValue::Uint(u) => Ok(JsonValue::Number((*u).into())),
            automerge::ScalarValue::F64(f) => {
                let n = serde_json::Number::from_f64(*f)
                    .ok_or_else(|| Error::document("invalid float value"))?;
                Ok(JsonValue::Number(n))
            }
            automerge::ScalarValue::Str(s) => {
                // Try to parse as JSON first (for complex types)
                if let Ok(parsed) = serde_json::from_str(s) {
                    Ok(parsed)
                } else {
                    Ok(JsonValue::String(s.to_string()))
                }
            }
            automerge::ScalarValue::Bytes(b) => {
                // Encode bytes as base64
                use base64::Engine;
                let encoded = base64::engine::general_purpose::STANDARD.encode(b);
                Ok(JsonValue::String(encoded))
            }
            _ => Err(Error::document("unsupported Automerge value type")),
        },
        Value::Object(_) => {
            // TODO: Handle nested objects/lists properly
            Err(Error::document("nested objects/lists not yet supported"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn create_test_doc() -> (TempDir, Document) {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("test.doc");
        let doc = AutoCommit::new();
        let document = Document::new("test".to_string(), path, doc);
        (temp_dir, document)
    }

    #[test]
    fn test_put_and_get_string() {
        let (_dir, doc) = create_test_doc();

        doc.put("name", "Alice").unwrap();
        let name: Option<String> = doc.get("name").unwrap();
        assert_eq!(name, Some("Alice".to_string()));
    }

    #[test]
    fn test_put_and_get_number() {
        let (_dir, doc) = create_test_doc();

        doc.put("count", 42i64).unwrap();
        let count: Option<i64> = doc.get("count").unwrap();
        assert_eq!(count, Some(42));
    }

    #[test]
    fn test_put_and_get_bool() {
        let (_dir, doc) = create_test_doc();

        doc.put("active", true).unwrap();
        let active: Option<bool> = doc.get("active").unwrap();
        assert_eq!(active, Some(true));
    }

    #[test]
    fn test_get_nonexistent() {
        let (_dir, doc) = create_test_doc();

        let value: Option<String> = doc.get("nonexistent").unwrap();
        assert_eq!(value, None);
    }

    #[test]
    fn test_delete() {
        let (_dir, doc) = create_test_doc();

        doc.put("key", "value").unwrap();
        assert!(doc.contains("key").unwrap());

        doc.delete("key").unwrap();
        assert!(!doc.contains("key").unwrap());
    }

    #[test]
    fn test_keys() {
        let (_dir, doc) = create_test_doc();

        doc.put("a", 1i64).unwrap();
        doc.put("b", 2i64).unwrap();
        doc.put("c", 3i64).unwrap();

        let mut keys = doc.keys().unwrap();
        keys.sort();
        assert_eq!(keys, vec!["a", "b", "c"]);
    }

    #[test]
    fn test_save_and_dirty() {
        let (dir, doc) = create_test_doc();

        assert!(!doc.is_dirty());

        doc.put("key", "value").unwrap();
        assert!(doc.is_dirty());

        doc.save().unwrap();
        assert!(!doc.is_dirty());
        assert!(dir.path().join("test.doc").exists());
    }

    #[test]
    fn test_get_text_nonexistent() {
        let (_dir, doc) = create_test_doc();
        assert_eq!(doc.get_text("missing").unwrap(), None);
    }

    #[test]
    fn test_ensure_text_creates() {
        let (_dir, doc) = create_test_doc();
        doc.ensure_text("content").unwrap();
        assert_eq!(doc.get_text("content").unwrap(), Some(String::new()));
    }

    #[test]
    fn test_update_text_basic() {
        let (_dir, doc) = create_test_doc();
        doc.update_text("content", "hello world").unwrap();
        assert_eq!(doc.get_text("content").unwrap(), Some("hello world".to_string()));
    }

    #[test]
    fn test_update_text_diff() {
        let (_dir, doc) = create_test_doc();
        doc.update_text("content", "hello world").unwrap();
        doc.update_text("content", "hello brave world").unwrap();
        assert_eq!(doc.get_text("content").unwrap(), Some("hello brave world".to_string()));
    }

    #[test]
    fn test_scalar_to_text_migration() {
        let (_dir, doc) = create_test_doc();
        doc.put("content", "old text").unwrap();
        // get_text should still read the scalar
        assert_eq!(doc.get_text("content").unwrap(), Some("old text".to_string()));
        // update_text should migrate and work
        doc.update_text("content", "new text").unwrap();
        assert_eq!(doc.get_text("content").unwrap(), Some("new text".to_string()));
    }

    #[test]
    fn test_set_and_get_cursors() {
        let (_dir, doc) = create_test_doc();
        doc.set_cursor("device1", 10, "Alice's Mac", "typing").unwrap();
        doc.set_cursor("device2", 20, "Bob's iPad", "viewing").unwrap();

        let cursors = doc.get_cursors("device1").unwrap();
        assert_eq!(cursors.len(), 1);
        assert_eq!(cursors[0].device_id, "device2");
        assert_eq!(cursors[0].position, 20);

        let all = doc.get_cursors("none").unwrap();
        assert_eq!(all.len(), 2);
    }
}
