//! Device registry database

use std::path::Path;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use iroh::NodeId;
use parking_lot::Mutex;
use rusqlite::Connection;
use tracing::debug;

/// Device information stored in the database
#[derive(Debug, Clone)]
pub struct DeviceRecord {
    /// Device's Iroh NodeId
    pub node_id: NodeId,
    /// Human-readable device name
    pub name: String,
    /// When the device was paired
    pub paired_at: DateTime<Utc>,
    /// When the device was last seen
    pub last_seen: DateTime<Utc>,
}

/// SQLite database for device registry
pub struct Database {
    conn: Mutex<Connection>,
}

impl Database {
    /// Open or create the database
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path).context("Failed to open SQLite database")?;

        // Create tables
        conn.execute(
            "CREATE TABLE IF NOT EXISTS devices (
                node_id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                paired_at TEXT NOT NULL,
                last_seen TEXT NOT NULL
            )",
            [],
        )
        .context("Failed to create devices table")?;

        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Add a device to the registry
    pub fn add_device(&self, node_id: NodeId, name: &str) -> Result<DeviceRecord> {
        let now = Utc::now();
        let node_id_str = node_id.to_string();

        let conn = self.conn.lock();
        conn.execute(
            "INSERT OR REPLACE INTO devices (node_id, name, paired_at, last_seen)
             VALUES (?1, ?2, ?3, ?4)",
            [&node_id_str, name, &now.to_rfc3339(), &now.to_rfc3339()],
        )
        .context("Failed to insert device")?;

        debug!(node_id = %node_id, name, "Device added to registry");

        Ok(DeviceRecord {
            node_id,
            name: name.to_string(),
            paired_at: now,
            last_seen: now,
        })
    }

    /// Remove a device from the registry
    pub fn remove_device(&self, node_id: &NodeId) -> Result<bool> {
        let node_id_str = node_id.to_string();

        let conn = self.conn.lock();
        let rows = conn
            .execute("DELETE FROM devices WHERE node_id = ?1", [&node_id_str])
            .context("Failed to delete device")?;

        debug!(node_id = %node_id, removed = rows > 0, "Device removal attempted");
        Ok(rows > 0)
    }

    /// Check if a device is registered
    pub fn contains(&self, node_id: &NodeId) -> Result<bool> {
        let node_id_str = node_id.to_string();

        let conn = self.conn.lock();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM devices WHERE node_id = ?1",
                [&node_id_str],
                |row| row.get(0),
            )
            .context("Failed to check device")?;

        Ok(count > 0)
    }

    /// Get all registered devices
    pub fn list_devices(&self) -> Result<Vec<DeviceRecord>> {
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare("SELECT node_id, name, paired_at, last_seen FROM devices")
            .context("Failed to prepare statement")?;

        let devices = stmt
            .query_map([], |row| {
                let node_id_str: String = row.get(0)?;
                let name: String = row.get(1)?;
                let paired_at_str: String = row.get(2)?;
                let last_seen_str: String = row.get(3)?;

                Ok((node_id_str, name, paired_at_str, last_seen_str))
            })
            .context("Failed to query devices")?;

        let mut result = Vec::new();
        for device in devices {
            let (node_id_str, name, paired_at_str, last_seen_str) =
                device.context("Failed to read device row")?;

            let node_id = node_id_str
                .parse()
                .context("Failed to parse node_id")?;
            let paired_at = DateTime::parse_from_rfc3339(&paired_at_str)
                .context("Failed to parse paired_at")?
                .with_timezone(&Utc);
            let last_seen = DateTime::parse_from_rfc3339(&last_seen_str)
                .context("Failed to parse last_seen")?
                .with_timezone(&Utc);

            result.push(DeviceRecord {
                node_id,
                name,
                paired_at,
                last_seen,
            });
        }

        Ok(result)
    }

    /// Update the last seen time for a device
    pub fn touch_device(&self, node_id: &NodeId) -> Result<bool> {
        let node_id_str = node_id.to_string();
        let now = Utc::now().to_rfc3339();

        let conn = self.conn.lock();
        let rows = conn
            .execute(
                "UPDATE devices SET last_seen = ?1 WHERE node_id = ?2",
                [&now, &node_id_str],
            )
            .context("Failed to update device")?;

        Ok(rows > 0)
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

    #[test]
    fn test_database_crud() {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test.db");
        let db = Database::open(&db_path).unwrap();

        let node_id = test_node_id();

        // Add device
        let record = db.add_device(node_id, "Test Device").unwrap();
        assert_eq!(record.name, "Test Device");

        // Check contains
        assert!(db.contains(&node_id).unwrap());
        assert!(!db.contains(&test_node_id_2()).unwrap());

        // List devices
        let devices = db.list_devices().unwrap();
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].name, "Test Device");

        // Remove device
        assert!(db.remove_device(&node_id).unwrap());
        assert!(!db.contains(&node_id).unwrap());
    }

    #[test]
    fn test_touch_device() {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test.db");
        let db = Database::open(&db_path).unwrap();

        let node_id = test_node_id();
        db.add_device(node_id, "Test").unwrap();

        std::thread::sleep(std::time::Duration::from_millis(10));

        assert!(db.touch_device(&node_id).unwrap());

        let devices = db.list_devices().unwrap();
        assert!(devices[0].last_seen > devices[0].paired_at);
    }
}
