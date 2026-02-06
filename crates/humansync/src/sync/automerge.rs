//! Automerge document sync over Iroh QUIC streams
//!
//! This module implements the Automerge sync protocol over Iroh QUIC bidirectional
//! streams. The protocol exchanges sync messages between peers until both documents
//! converge to the same state.
//!
//! # Protocol
//!
//! The sync protocol uses a simple message framing format:
//! - Each message is prefixed with a 4-byte big-endian length
//! - Message type is encoded in the first byte of the payload:
//!   - 0x01: Sync message
//!   - 0x02: Sync complete (no more messages)
//!   - 0x03: Error
//!
//! # Flow
//!
//! 1. Initiator opens a bidirectional stream
//! 2. Both peers exchange sync messages using Automerge's sync protocol
//! 3. Each peer generates messages until `generate_sync_message` returns `None`
//! 4. Sync completes when both peers have no more messages to send

use iroh::endpoint::Connection;
use tracing::{debug, trace, warn};

use crate::doc::Document;
use crate::error::{Error, Result};

/// Maximum sync message size (16 MB)
const MAX_MESSAGE_SIZE: usize = 16 * 1024 * 1024;

/// Protocol tag byte for doc list request
pub const DOC_LIST_REQUEST: u8 = 0x20;

/// Maximum number of sync rounds to prevent infinite loops
const MAX_SYNC_ROUNDS: usize = 100;

/// Message types for the sync protocol
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageType {
    /// A sync message containing Automerge sync data
    SyncMessage = 0x01,
    /// Indicates sync is complete from this peer's perspective
    SyncComplete = 0x02,
    /// An error occurred during sync
    Error = 0x03,
}

impl TryFrom<u8> for MessageType {
    type Error = Error;

    fn try_from(value: u8) -> Result<Self> {
        match value {
            0x01 => Ok(Self::SyncMessage),
            0x02 => Ok(Self::SyncComplete),
            0x03 => Ok(Self::Error),
            _ => Err(Error::sync(format!("unknown message type: {value}"))),
        }
    }
}

/// Initiate sync with a remote peer
///
/// This function:
/// 1. Opens a bidirectional stream
/// 2. Exchanges Automerge sync messages until both documents converge
/// 3. Returns success when sync is complete
///
/// The caller is responsible for saving the document after sync completes.
pub async fn sync_docs(conn: &Connection, local_doc: &Document) -> Result<()> {
    debug!(doc = local_doc.name(), "Initiating document sync");

    // Open a bidirectional stream for sync
    let (mut send, recv) = conn
        .open_bi()
        .await
        .map_err(|e| Error::sync(format!("failed to open stream: {e}")))?;

    // Send doc name header so the server knows which document to sync.
    // Format: [4-byte big-endian name length][name bytes]
    let name_bytes = local_doc.name().as_bytes();
    let name_len = (name_bytes.len() as u32).to_be_bytes();
    send.write_all(&name_len)
        .await
        .map_err(|e| Error::sync(format!("failed to send doc name length: {e}")))?;
    send.write_all(name_bytes)
        .await
        .map_err(|e| Error::sync(format!("failed to send doc name: {e}")))?;

    // Run the sync protocol
    run_sync_protocol(send, recv, local_doc).await?;

    debug!(doc = local_doc.name(), "Document sync complete (initiator)");
    Ok(())
}

/// Handle an incoming sync request from a remote peer
///
/// This function:
/// 1. Accepts the bidirectional stream
/// 2. Exchanges Automerge sync messages until both documents converge
/// 3. Returns success when sync is complete
///
/// The caller is responsible for saving the document after sync completes.
pub async fn handle_sync_request(conn: &Connection, local_doc: &Document) -> Result<()> {
    debug!(doc = local_doc.name(), "Handling incoming sync request");

    // Accept the bidirectional stream
    let (send, mut recv) = conn
        .accept_bi()
        .await
        .map_err(|e| Error::sync(format!("failed to accept stream: {e}")))?;

    // Read and discard the doc name header sent by the initiator.
    // Format: [4-byte big-endian name length][name bytes]
    let mut len_buf = [0u8; 4];
    recv.read_exact(&mut len_buf)
        .await
        .map_err(|e| Error::sync(format!("failed to read doc name length: {e}")))?;
    let name_len = u32::from_be_bytes(len_buf) as usize;
    if name_len > 0 && name_len <= 4096 {
        let mut name_buf = vec![0u8; name_len];
        recv.read_exact(&mut name_buf)
            .await
            .map_err(|e| Error::sync(format!("failed to read doc name: {e}")))?;
    }

    // Run the sync protocol
    run_sync_protocol(send, recv, local_doc).await?;

    debug!(
        doc = local_doc.name(),
        "Document sync complete (responder)"
    );
    Ok(())
}

/// Run the Automerge sync protocol over a bidirectional stream
///
/// This implements the core sync loop that exchanges messages until both
/// peers have converged to the same document state.
///
/// This is public so that server-side code can run the sync protocol on
/// already-accepted streams (e.g., after routing based on a protocol tag byte).
pub async fn run_sync_protocol(
    mut send: iroh::endpoint::SendStream,
    mut recv: iroh::endpoint::RecvStream,
    local_doc: &Document,
) -> Result<()> {
    // Create a new sync state for this peer
    let mut sync_state = local_doc.new_sync_state();

    let mut rounds = 0;
    let mut local_done = false;
    let mut remote_done = false;

    while rounds < MAX_SYNC_ROUNDS && !(local_done && remote_done) {
        rounds += 1;
        trace!(round = rounds, "Sync round");

        // Generate our sync message
        let local_message = local_doc.generate_sync_message(&mut sync_state);

        match local_message {
            Some(msg) => {
                // Send our sync message
                let encoded = msg.encode();
                send_message(&mut send, MessageType::SyncMessage, &encoded).await?;
                trace!(size = encoded.len(), "Sent sync message");
            }
            None => {
                if !local_done {
                    // No more messages to send - notify peer
                    send_message(&mut send, MessageType::SyncComplete, &[]).await?;
                    local_done = true;
                    trace!("Local sync complete");
                }
            }
        }

        // Receive remote message
        let (msg_type, payload) = recv_message(&mut recv).await?;

        match msg_type {
            MessageType::SyncMessage => {
                // Decode and apply the sync message
                let remote_msg = automerge::sync::Message::decode(&payload)
                    .map_err(|e| Error::sync(format!("failed to decode sync message: {e}")))?;
                local_doc.receive_sync_message(&mut sync_state, remote_msg)?;
                trace!(size = payload.len(), "Received and applied sync message");
            }
            MessageType::SyncComplete => {
                remote_done = true;
                trace!("Remote sync complete");
            }
            MessageType::Error => {
                let error_msg = String::from_utf8_lossy(&payload);
                return Err(Error::sync(format!("remote error: {error_msg}")));
            }
        }
    }

    if rounds >= MAX_SYNC_ROUNDS {
        warn!(doc = local_doc.name(), "Sync reached max rounds limit");
        return Err(Error::sync("sync reached max rounds limit"));
    }

    // Finish the send stream
    send.finish()
        .map_err(|e| Error::sync(format!("failed to finish stream: {e}")))?;

    Ok(())
}

/// Send a framed message over the stream
///
/// Format: [4-byte length BE][1-byte type][payload]
async fn send_message(
    send: &mut iroh::endpoint::SendStream,
    msg_type: MessageType,
    payload: &[u8],
) -> Result<()> {
    let total_len = 1 + payload.len(); // 1 byte for type + payload

    if total_len > MAX_MESSAGE_SIZE {
        return Err(Error::sync("message too large"));
    }

    // Send length prefix
    let len_bytes = (total_len as u32).to_be_bytes();
    send.write_all(&len_bytes)
        .await
        .map_err(|e| Error::sync(format!("failed to send length: {e}")))?;

    // Send message type
    send.write_all(&[msg_type as u8])
        .await
        .map_err(|e| Error::sync(format!("failed to send message type: {e}")))?;

    // Send payload
    if !payload.is_empty() {
        send.write_all(payload)
            .await
            .map_err(|e| Error::sync(format!("failed to send payload: {e}")))?;
    }

    Ok(())
}

/// Receive a framed message from the stream
///
/// Returns the message type and payload
async fn recv_message(recv: &mut iroh::endpoint::RecvStream) -> Result<(MessageType, Vec<u8>)> {
    // Read length prefix
    let mut len_buf = [0u8; 4];
    recv.read_exact(&mut len_buf)
        .await
        .map_err(|e| Error::sync(format!("failed to read length: {e}")))?;
    let total_len = u32::from_be_bytes(len_buf) as usize;

    if total_len == 0 {
        return Err(Error::sync("received empty message"));
    }

    if total_len > MAX_MESSAGE_SIZE {
        return Err(Error::sync(format!(
            "message too large: {total_len} bytes (max {MAX_MESSAGE_SIZE})"
        )));
    }

    // Read message type
    let mut type_buf = [0u8; 1];
    recv.read_exact(&mut type_buf)
        .await
        .map_err(|e| Error::sync(format!("failed to read message type: {e}")))?;
    let msg_type = MessageType::try_from(type_buf[0])?;

    // Read payload
    let payload_len = total_len - 1;
    let payload = if payload_len > 0 {
        let mut payload = vec![0u8; payload_len];
        recv.read_exact(&mut payload)
            .await
            .map_err(|e| Error::sync(format!("failed to read payload: {e}")))?;
        payload
    } else {
        Vec::new()
    };

    Ok((msg_type, payload))
}

/// Request a list of document names from a remote peer.
///
/// Opens a bidirectional stream, sends the `DOC_LIST_REQUEST` tag,
/// and reads back the peer's document list.
pub async fn request_doc_list(conn: &Connection) -> Result<Vec<String>> {
    debug!("Requesting doc list from peer");

    let (mut send, mut recv) = conn
        .open_bi()
        .await
        .map_err(|e| Error::sync(format!("failed to open stream for doc list: {e}")))?;

    // Send the doc list request tag
    send.write_all(&[DOC_LIST_REQUEST])
        .await
        .map_err(|e| Error::sync(format!("failed to send doc list request: {e}")))?;

    // Signal we're done sending
    send.finish()
        .map_err(|e| Error::sync(format!("failed to finish doc list request stream: {e}")))?;

    // Read the response: 4-byte count
    let mut count_buf = [0u8; 4];
    recv.read_exact(&mut count_buf)
        .await
        .map_err(|e| Error::sync(format!("failed to read doc list count: {e}")))?;
    let count = u32::from_be_bytes(count_buf) as usize;

    if count > 10_000 {
        return Err(Error::sync(format!("doc list too large: {count}")));
    }

    let mut doc_names = Vec::with_capacity(count);
    for _ in 0..count {
        let mut len_buf = [0u8; 4];
        recv.read_exact(&mut len_buf)
            .await
            .map_err(|e| Error::sync(format!("failed to read doc name length: {e}")))?;
        let name_len = u32::from_be_bytes(len_buf) as usize;

        if name_len == 0 || name_len > 4096 {
            return Err(Error::sync(format!("invalid doc name length: {name_len}")));
        }

        let mut name_buf = vec![0u8; name_len];
        recv.read_exact(&mut name_buf)
            .await
            .map_err(|e| Error::sync(format!("failed to read doc name: {e}")))?;
        let name = String::from_utf8(name_buf)
            .map_err(|e| Error::sync(format!("invalid doc name encoding: {e}")))?;
        doc_names.push(name);
    }

    debug!(count = doc_names.len(), "Received doc list from peer");
    Ok(doc_names)
}

/// Handle an incoming doc list request from a remote peer.
///
/// Sends back the provided list of document names.
///
/// Response format: `[4-byte BE count][for each: 4-byte BE name length, name bytes]`
pub async fn handle_doc_list_request(
    mut send: iroh::endpoint::SendStream,
    doc_names: &[String],
) -> Result<()> {
    let count = (doc_names.len() as u32).to_be_bytes();
    send.write_all(&count)
        .await
        .map_err(|e| Error::sync(format!("failed to send doc list count: {e}")))?;

    for name in doc_names {
        let name_bytes = name.as_bytes();
        let name_len = (name_bytes.len() as u32).to_be_bytes();
        send.write_all(&name_len)
            .await
            .map_err(|e| Error::sync(format!("failed to send doc name length: {e}")))?;
        send.write_all(name_bytes)
            .await
            .map_err(|e| Error::sync(format!("failed to send doc name: {e}")))?;
    }

    send.finish()
        .map_err(|e| Error::sync(format!("failed to finish doc list response stream: {e}")))?;

    debug!(count = doc_names.len(), "Sent doc list to peer");
    Ok(())
}

/// Sync a document between two local documents (for testing)
///
/// This is useful for testing the sync protocol without network I/O.
#[cfg(test)]
pub fn sync_docs_local(doc1: &Document, doc2: &Document) -> Result<()> {
    let mut sync1 = doc1.new_sync_state();
    let mut sync2 = doc2.new_sync_state();

    for round in 0..MAX_SYNC_ROUNDS {
        let msg1 = doc1.generate_sync_message(&mut sync1);
        let msg2 = doc2.generate_sync_message(&mut sync2);

        if msg1.is_none() && msg2.is_none() {
            trace!(rounds = round, "Local sync complete");
            return Ok(());
        }

        if let Some(msg) = msg1 {
            doc2.receive_sync_message(&mut sync2, msg)?;
        }
        if let Some(msg) = msg2 {
            doc1.receive_sync_message(&mut sync1, msg)?;
        }
    }

    Err(Error::sync("local sync reached max rounds limit"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::AutoCommit;
    use tempfile::TempDir;

    fn create_test_doc(name: &str) -> (TempDir, Document) {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join(format!("{name}.doc"));
        let doc = AutoCommit::new();
        let document = Document::new(name.to_string(), path, doc);
        (temp_dir, document)
    }

    #[test]
    fn test_sync_empty_docs() {
        let (_dir1, doc1) = create_test_doc("doc1");
        let (_dir2, doc2) = create_test_doc("doc2");

        sync_docs_local(&doc1, &doc2).unwrap();

        assert_eq!(doc1.get_heads(), doc2.get_heads());
    }

    #[test]
    fn test_sync_one_way() {
        let (_dir1, doc1) = create_test_doc("doc1");
        let (_dir2, doc2) = create_test_doc("doc2");

        // Only doc1 has changes
        doc1.put("key", "value").unwrap();

        sync_docs_local(&doc1, &doc2).unwrap();

        // Both should have the same data
        let val1: Option<String> = doc1.get("key").unwrap();
        let val2: Option<String> = doc2.get("key").unwrap();
        assert_eq!(val1, Some("value".to_string()));
        assert_eq!(val2, Some("value".to_string()));
        assert_eq!(doc1.get_heads(), doc2.get_heads());
    }

    #[test]
    fn test_sync_concurrent_edits() {
        let (_dir1, doc1) = create_test_doc("doc1");
        let (_dir2, doc2) = create_test_doc("doc2");

        // Both docs have different changes
        doc1.put("key1", "value1").unwrap();
        doc2.put("key2", "value2").unwrap();

        sync_docs_local(&doc1, &doc2).unwrap();

        // Both should have both keys
        let val1_1: Option<String> = doc1.get("key1").unwrap();
        let val1_2: Option<String> = doc1.get("key2").unwrap();
        let val2_1: Option<String> = doc2.get("key1").unwrap();
        let val2_2: Option<String> = doc2.get("key2").unwrap();

        assert_eq!(val1_1, Some("value1".to_string()));
        assert_eq!(val1_2, Some("value2".to_string()));
        assert_eq!(val2_1, Some("value1".to_string()));
        assert_eq!(val2_2, Some("value2".to_string()));
        assert_eq!(doc1.get_heads(), doc2.get_heads());
    }

    #[test]
    fn test_sync_conflict_resolution() {
        let (_dir1, doc1) = create_test_doc("doc1");
        let (_dir2, doc2) = create_test_doc("doc2");

        // Both docs modify the same key
        doc1.put("key", "value1").unwrap();
        doc2.put("key", "value2").unwrap();

        sync_docs_local(&doc1, &doc2).unwrap();

        // After sync, both should have the same value (Automerge picks one)
        let val1: Option<String> = doc1.get("key").unwrap();
        let val2: Option<String> = doc2.get("key").unwrap();

        assert_eq!(val1, val2);
        assert_eq!(doc1.get_heads(), doc2.get_heads());
    }

    #[test]
    fn test_multiple_sync_rounds() {
        let (_dir1, doc1) = create_test_doc("doc1");
        let (_dir2, doc2) = create_test_doc("doc2");

        // Add data, sync, add more data, sync again
        doc1.put("round1", "data1").unwrap();
        sync_docs_local(&doc1, &doc2).unwrap();

        doc2.put("round2", "data2").unwrap();
        sync_docs_local(&doc1, &doc2).unwrap();

        doc1.put("round3", "data3").unwrap();
        sync_docs_local(&doc1, &doc2).unwrap();

        // Both should have all data
        assert_eq!(
            doc1.get::<String>("round1").unwrap(),
            Some("data1".to_string())
        );
        assert_eq!(
            doc1.get::<String>("round2").unwrap(),
            Some("data2".to_string())
        );
        assert_eq!(
            doc1.get::<String>("round3").unwrap(),
            Some("data3".to_string())
        );
        assert_eq!(doc1.get_heads(), doc2.get_heads());
    }

    #[test]
    fn test_message_type_conversion() {
        assert_eq!(MessageType::try_from(0x01).unwrap(), MessageType::SyncMessage);
        assert_eq!(
            MessageType::try_from(0x02).unwrap(),
            MessageType::SyncComplete
        );
        assert_eq!(MessageType::try_from(0x03).unwrap(), MessageType::Error);
        assert!(MessageType::try_from(0xFF).is_err());
    }
}
