//! Device identity management
//!
//! Each device has a persistent cryptographic identity (Ed25519 keypair).
//! The public key serves as the device's NodeId in the Iroh network.

use std::path::Path;

use iroh::SecretKey;
use tokio::fs;
use tracing::{debug, info};

use crate::error::{Error, Result};

/// Load or create a persistent device identity
///
/// If a key file exists at the given path, it is loaded.
/// Otherwise, a new keypair is generated and saved.
pub async fn load_or_create_identity(path: &Path) -> Result<SecretKey> {
    if path.exists() {
        load_identity(path).await
    } else {
        create_identity(path).await
    }
}

/// Load an existing identity from disk
async fn load_identity(path: &Path) -> Result<SecretKey> {
    debug!(path = %path.display(), "Loading device identity");

    let bytes = fs::read(path)
        .await
        .map_err(|e| Error::identity(format!("failed to read key file: {e}")))?;

    let key_array: [u8; 32] = bytes
        .try_into()
        .map_err(|_| Error::identity("invalid key length (expected 32 bytes)"))?;

    let secret_key = SecretKey::from_bytes(&key_array);
    info!(node_id = %secret_key.public(), "Loaded existing device identity");

    Ok(secret_key)
}

/// Create a new identity and save to disk
async fn create_identity(path: &Path) -> Result<SecretKey> {
    debug!("Generating new device identity");

    let secret_key = SecretKey::generate(rand::thread_rng());

    // Ensure parent directory exists
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .await
            .map_err(|e| Error::identity(format!("failed to create directory: {e}")))?;
    }

    // Write key with restrictive permissions on Unix
    #[cfg(unix)]
    {
        write_key_unix(path, &secret_key).await?;
    }

    #[cfg(not(unix))]
    {
        fs::write(path, secret_key.to_bytes())
            .await
            .map_err(|e| Error::identity(format!("failed to write key file: {e}")))?;
    }

    info!(
        node_id = %secret_key.public(),
        path = %path.display(),
        "Generated new device identity"
    );

    Ok(secret_key)
}

/// Write key file with Unix permissions (0o600)
#[cfg(unix)]
async fn write_key_unix(path: &Path, secret_key: &SecretKey) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;

    let path = path.to_owned();
    let key_bytes = secret_key.to_bytes();

    tokio::task::spawn_blocking(move || {
        use std::io::Write;

        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&path)
            .map_err(|e| Error::identity(format!("failed to create key file: {e}")))?;

        file.write_all(&key_bytes)
            .map_err(|e| Error::identity(format!("failed to write key: {e}")))?;

        file.flush()
            .map_err(|e| Error::identity(format!("failed to flush key file: {e}")))?;

        Ok(())
    })
    .await
    .map_err(|e| Error::identity(format!("key write task failed: {e}")))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_create_and_load_identity() {
        let temp_dir = TempDir::new().unwrap();
        let key_path = temp_dir.path().join("device_key");

        // First call creates identity
        let key1 = load_or_create_identity(&key_path).await.unwrap();
        assert!(key_path.exists());

        // Second call loads same identity
        let key2 = load_or_create_identity(&key_path).await.unwrap();
        assert_eq!(key1.to_bytes(), key2.to_bytes());
        assert_eq!(key1.public(), key2.public());
    }

    #[tokio::test]
    async fn test_invalid_key_file() {
        let temp_dir = TempDir::new().unwrap();
        let key_path = temp_dir.path().join("device_key");

        // Write invalid key (wrong length)
        fs::write(&key_path, b"too short").await.unwrap();

        let result = load_or_create_identity(&key_path).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("invalid key length"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_key_file_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let temp_dir = TempDir::new().unwrap();
        let key_path = temp_dir.path().join("device_key");

        load_or_create_identity(&key_path).await.unwrap();

        let metadata = fs::metadata(&key_path).await.unwrap();
        let mode = metadata.permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "Key file should have mode 0o600");
    }

    #[tokio::test]
    async fn test_identity_persistence_across_restarts() {
        let temp_dir = TempDir::new().unwrap();
        let key_path = temp_dir.path().join("device_key");

        // Simulate multiple "restarts" by loading identity multiple times
        let original_key = load_or_create_identity(&key_path).await.unwrap();
        let original_node_id = original_key.public();

        // Simulate 5 restarts
        for i in 1..=5 {
            let loaded_key = load_or_create_identity(&key_path).await.unwrap();
            assert_eq!(
                loaded_key.public(),
                original_node_id,
                "NodeId should be consistent after restart {i}"
            );
            assert_eq!(
                loaded_key.to_bytes(),
                original_key.to_bytes(),
                "Secret key bytes should be identical after restart {i}"
            );
        }
    }

    #[tokio::test]
    async fn test_node_id_derived_correctly() {
        let temp_dir = TempDir::new().unwrap();
        let key_path = temp_dir.path().join("device_key");

        let secret_key = load_or_create_identity(&key_path).await.unwrap();
        let node_id = secret_key.public();

        // NodeId should be a valid 32-byte public key
        let node_id_str = node_id.to_string();
        assert!(!node_id_str.is_empty(), "NodeId string should not be empty");

        // Verify the public key can be formatted
        let formatted = format!("{}", node_id);
        assert!(formatted.len() > 10, "NodeId should have reasonable string representation");
    }

    #[tokio::test]
    async fn test_creates_parent_directories() {
        let temp_dir = TempDir::new().unwrap();
        let nested_path = temp_dir
            .path()
            .join("deeply")
            .join("nested")
            .join("path")
            .join("device_key");

        // Should create all parent directories
        let key = load_or_create_identity(&nested_path).await.unwrap();
        assert!(nested_path.exists());
        assert!(nested_path.parent().unwrap().exists());

        // Verify key is valid
        let _node_id = key.public();
    }

    #[tokio::test]
    async fn test_different_paths_create_different_identities() {
        let temp_dir = TempDir::new().unwrap();
        let key_path1 = temp_dir.path().join("device_key_1");
        let key_path2 = temp_dir.path().join("device_key_2");

        let key1 = load_or_create_identity(&key_path1).await.unwrap();
        let key2 = load_or_create_identity(&key_path2).await.unwrap();

        // Different key files should produce different identities
        assert_ne!(
            key1.public(),
            key2.public(),
            "Different key files should produce different NodeIds"
        );
        assert_ne!(
            key1.to_bytes(),
            key2.to_bytes(),
            "Different key files should have different secret keys"
        );
    }

    #[tokio::test]
    async fn test_exact_32_byte_key() {
        let temp_dir = TempDir::new().unwrap();
        let key_path = temp_dir.path().join("device_key");

        // Create a key
        load_or_create_identity(&key_path).await.unwrap();

        // Read raw bytes
        let key_bytes = fs::read(&key_path).await.unwrap();
        assert_eq!(
            key_bytes.len(),
            32,
            "Key file should be exactly 32 bytes (Ed25519 secret key)"
        );
    }
}
