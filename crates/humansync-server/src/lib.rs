//! HumanSync Server library
//!
//! Re-exports the server modules for use by the binary and integration tests.

use std::sync::Arc;

use humansync::DeviceRegistryStore;
use iroh::NodeId;

pub mod api;
pub mod db;
pub mod peer;

/// Shared application state
pub struct AppState {
    /// The cloud peer (Iroh node). None in test environments.
    pub peer: Option<peer::CloudPeer>,
    /// Device registry database
    pub db: Arc<db::Database>,
    /// Server password for pairing
    pub password: String,
    /// Device registry store (Automerge-backed, for sync propagation)
    pub registry_store: DeviceRegistryStore,
    /// Server's NodeId (cached for API responses)
    pub node_id: NodeId,
}
