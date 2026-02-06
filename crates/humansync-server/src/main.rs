//! HumanSync Server
//!
//! The always-on cloud peer and device registry for HumanSync.
//!
//! ## Features
//!
//! - **Cloud Peer**: Always-on Iroh node that participates in sync
//! - **Device Registry**: Tracks which devices belong to this user
//! - **Pairing API**: HTTP endpoint for device registration

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Parser;
use humansync::DeviceRegistryStore;
use tracing::{info, Level};
use tracing_subscriber::FmtSubscriber;

use humansync_server::{api, db::Database, peer::CloudPeer, AppState};

/// HumanSync Server - Cloud peer and device registry
#[derive(Parser, Debug)]
#[command(name = "humansync-server")]
#[command(version, about, long_about = None)]
struct Args {
    /// Server password for device pairing
    #[arg(long, env = "HUMANSYNC_PASSWORD")]
    password: String,

    /// Data directory for persistence
    #[arg(long, default_value = "/data", env = "HUMANSYNC_DATA_DIR")]
    data_dir: PathBuf,

    /// Iroh QUIC port
    #[arg(long, default_value = "4433", env = "HUMANSYNC_IROH_PORT")]
    iroh_port: u16,

    /// HTTP API port
    #[arg(long, default_value = "8080", env = "HUMANSYNC_API_PORT")]
    api_port: u16,

    /// Log level
    #[arg(long, default_value = "info", env = "HUMANSYNC_LOG_LEVEL")]
    log_level: Level,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    // Initialize logging
    let subscriber = FmtSubscriber::builder()
        .with_max_level(args.log_level)
        .with_target(true)
        .with_thread_ids(false)
        .with_file(false)
        .with_line_number(false)
        .finish();
    tracing::subscriber::set_global_default(subscriber)
        .context("Failed to set tracing subscriber")?;

    info!("Starting HumanSync Server");
    info!(data_dir = %args.data_dir.display(), "Data directory");
    info!(iroh_port = args.iroh_port, "Iroh QUIC port");
    info!(api_port = args.api_port, "HTTP API port");

    // Ensure data directory exists
    tokio::fs::create_dir_all(&args.data_dir)
        .await
        .context("Failed to create data directory")?;

    // Initialize database
    let db_path = args.data_dir.join("devices.db");
    let db = Arc::new(Database::open(&db_path).context("Failed to open database")?);
    info!(path = %db_path.display(), "Database initialized");

    // Initialize cloud peer
    let peer = CloudPeer::new(&args.data_dir, args.iroh_port)
        .await
        .context("Failed to initialize cloud peer")?;
    info!(node_id = %peer.node_id(), "Cloud peer initialized");

    // Start the accept loop for incoming sync connections (gated by device registry)
    peer.start_accept_loop(db.clone());
    info!("Cloud peer accept loop started");

    // Initialize the device registry store (Automerge-backed)
    let registry_store = DeviceRegistryStore::new(&args.data_dir)
        .context("Failed to initialize device registry store")?;
    info!("Device registry store initialized");

    // Create shared state
    let node_id = peer.node_id();
    let state = Arc::new(AppState {
        peer: Some(peer),
        db,
        password: args.password,
        registry_store,
        node_id,
    });

    // Start HTTP API server
    let api_addr: SocketAddr = ([0, 0, 0, 0], args.api_port).into();
    let app = api::router(state.clone());

    info!(addr = %api_addr, "Starting HTTP API server");

    let listener = tokio::net::TcpListener::bind(api_addr)
        .await
        .context("Failed to bind API server")?;

    // Run with graceful shutdown on ctrl-c
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("API server error")?;

    info!("HumanSync Server shutting down");
    Ok(())
}

/// Wait for a ctrl-c signal for graceful shutdown
async fn shutdown_signal() {
    tokio::signal::ctrl_c()
        .await
        .expect("Failed to install ctrl-c signal handler");
    info!("Received ctrl-c, initiating graceful shutdown");
}
