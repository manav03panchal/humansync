//! Basic usage example for HumanSync.
//!
//! This example demonstrates the core API for using HumanSync:
//! - Initializing a node
//! - Opening and working with documents
//! - Storing and retrieving blobs
//! - Checking sync status
//! - Graceful shutdown
//!
//! Run with: cargo run --example basic_usage

use std::path::Path;

use humansync::{Config, HumanSync};

#[tokio::main]
async fn main() -> humansync::Result<()> {
    // Initialize logging for visibility
    tracing_subscriber::fmt::init();

    println!("=== HumanSync Basic Usage Example ===\n");

    // -------------------------------------------------------------------------
    // Step 1: Initialize the HumanSync node
    // -------------------------------------------------------------------------
    println!("1. Initializing HumanSync node...");

    // Create configuration with a temporary storage path for this example
    // In a real app, use a persistent path like "~/.myapp/humansync"
    let storage_path = std::env::temp_dir().join("humansync-example");
    println!("   Storage path: {}", storage_path.display());

    let config = Config::new(&storage_path)
        .with_server_url("https://example.humansync.io") // Optional server
        .with_sync_interval(30); // Sync every 30 seconds

    let node = HumanSync::init(config).await?;

    println!("   Node ID: {}", node.node_id()?);
    println!("   Node initialized successfully!\n");

    // -------------------------------------------------------------------------
    // Step 2: Work with documents
    // -------------------------------------------------------------------------
    println!("2. Working with documents...");

    // Open or create a document
    let doc = node.open_doc("notes/shopping-list").await?;
    println!("   Opened document: {}", doc.name());

    // Write some data
    doc.put("eggs", 12_i64)?;
    doc.put("milk", true)?;
    doc.put("store", "Costco")?;
    println!("   Added items to shopping list");

    // Read data back
    let eggs: Option<i64> = doc.get("eggs")?;
    let milk: Option<bool> = doc.get("milk")?;
    let store: Option<String> = doc.get("store")?;

    println!("   Reading back:");
    println!("     - eggs: {:?}", eggs);
    println!("     - milk: {:?}", milk);
    println!("     - store: {:?}", store);

    // List all keys
    let keys = doc.keys()?;
    println!("   All keys: {:?}", keys);

    // Save the document to disk
    doc.save()?;
    println!("   Document saved!\n");

    // -------------------------------------------------------------------------
    // Step 3: Work with blobs (large files)
    // -------------------------------------------------------------------------
    println!("3. Working with blobs...");

    // Store some bytes as a blob
    let sample_data = b"This is sample file content for the blob example";
    let blob_hash = node.store_blob_bytes(sample_data.to_vec()).await?;
    println!("   Stored blob with hash: {}...", &blob_hash[..16]);

    // Check blob exists
    let exists = node.has_blob(&blob_hash).await?;
    println!("   Blob exists locally: {}", exists);

    // Get blob size
    let size = node.blob_size(&blob_hash).await?;
    println!("   Blob size: {} bytes", size);

    // Retrieve blob content
    let retrieved = node.get_blob_bytes(&blob_hash).await?;
    println!(
        "   Retrieved content matches: {}",
        &retrieved[..] == sample_data
    );

    // Reference the blob from a document
    doc.put("attachment", &blob_hash)?;
    doc.save()?;
    println!("   Referenced blob from document!\n");

    // -------------------------------------------------------------------------
    // Step 4: Store a file as a blob (if exists)
    // -------------------------------------------------------------------------
    println!("4. Storing files as blobs...");

    // Create a sample file for demonstration
    let sample_file = storage_path.join("sample-photo.txt");
    tokio::fs::write(&sample_file, b"Pretend this is a photo")
        .await
        .expect("failed to write sample file");

    let file_hash = node.store_blob(Path::new(&sample_file)).await?;
    println!("   Stored file with hash: {}...", &file_hash[..16]);

    // Get the path to the blob (useful for opening in external apps)
    let blob_path = node.get_blob(&file_hash).await?;
    println!("   Blob path: {}\n", blob_path.display());

    // -------------------------------------------------------------------------
    // Step 5: List documents
    // -------------------------------------------------------------------------
    println!("5. Listing documents...");

    // Create a few more documents
    let _doc2 = node.open_doc("notes/todo").await?;
    let _doc3 = node.open_doc("tasks/work").await?;

    // List documents in the "notes/" namespace
    let notes = node.list_docs("notes/")?;
    println!("   Documents in notes/: {:?}", notes);

    // List all documents
    let all_docs = node.list_docs("")?;
    println!("   All documents: {:?}\n", all_docs);

    // -------------------------------------------------------------------------
    // Step 6: Check sync status
    // -------------------------------------------------------------------------
    println!("6. Checking sync status...");

    let status = node.sync_status();
    println!("   Peers connected: {}", status.peers_connected);
    println!("   Documents pending: {}", status.docs_pending);
    match status.last_sync {
        Some(ts) => println!("   Last sync: {}", ts),
        None => println!("   Last sync: never (no peers connected yet)"),
    }
    println!();

    // -------------------------------------------------------------------------
    // Step 7: Graceful shutdown
    // -------------------------------------------------------------------------
    println!("7. Shutting down...");

    // Check node is running
    println!("   Node running: {}", node.is_running());

    // Shutdown gracefully
    node.shutdown().await?;

    println!("   Node running: {}", node.is_running());
    println!("   Shutdown complete!\n");

    // -------------------------------------------------------------------------
    // Cleanup
    // -------------------------------------------------------------------------
    println!("8. Cleaning up example data...");
    tokio::fs::remove_dir_all(&storage_path).await.ok();
    println!("   Removed temporary storage\n");

    println!("=== Example Complete ===");

    Ok(())
}
