# Iroh 0.92.0 API Notes

This document captures API observations, issues, and workarounds discovered while implementing HumanSync with iroh 0.92.0.

## Overview

- **iroh version**: 0.92.0
- **iroh-blobs version**: 0.92.0
- **Date**: 2026-02-05

## Key API Patterns

### Endpoint Creation

```rust
use iroh::{Endpoint, RelayMode, SecretKey};

// Basic endpoint with n0 discovery (production)
let endpoint = Endpoint::builder()
    .secret_key(secret_key)
    .alpns(vec![b"my-alpn".to_vec()])
    .discovery_n0()
    .bind()
    .await?;

// Endpoint with relay disabled (testing)
let endpoint = Endpoint::builder()
    .secret_key(secret_key)
    .alpns(vec![b"my-alpn".to_vec()])
    .relay_mode(RelayMode::Disabled)
    .bind()
    .await?;
```

### Getting Node Address with Watcher

The `node_addr()` method returns a `Watcher` that provides access to the current `NodeAddr`. Use `initialized()` to wait for the address to be ready:

```rust
use iroh::Watcher;

// Wait for node address to be available
let node_addr = endpoint.node_addr().initialized().await;

// Or get direct addresses directly
let direct_addrs = endpoint.direct_addresses().initialized().await;
```

**Note**: The `Watcher` pattern is new in recent Iroh versions. Don't try to `.await` the `node_addr()` result directly - use `.initialized().await` instead.

### Connection Establishment

```rust
// Connect to a peer
let conn = endpoint
    .connect(node_addr, b"alpn")
    .await?;

// Accept a connection
let incoming = endpoint.accept().await.expect("no incoming");
let conn = incoming.accept()?.await?;

// Note: Two-step accept pattern (accept() returns Incoming, then .accept() to start)
```

### QUIC Streams

```rust
// Open bidirectional stream
let (mut send, mut recv) = conn.open_bi().await?;

// Accept bidirectional stream
let (mut send, mut recv) = conn.accept_bi().await?;

// Send data
send.write_all(data).await?;
send.finish()?;  // Note: finish() is not async

// Receive data
let mut buf = vec![0u8; 1024];
let n = recv.read(&mut buf).await?.expect("no data");
```

## Issues and Workarounds

### 1. n0 Discovery Causes Long Startup/Shutdown Times

**Issue**: When using `discovery_n0()`, endpoint creation and `close()` can take 30+ seconds, especially in environments with network issues.

**Impact**: Tests timeout, development iteration is slow.

**Workaround**: For tests, use `RelayMode::Disabled`:
```rust
Endpoint::builder()
    .relay_mode(RelayMode::Disabled)
    .bind()
    .await?;
```

### 2. iroh-blobs TagInfo Field Access

**Issue**: In iroh-blobs 0.92.0, `TagInfo` has a `hash` field, not a `hash()` method.

**Incorrect**:
```rust
let hash = tag.hash();  // Error: not a method
```

**Correct**:
```rust
let hash = tag.hash;  // Access the field directly
```

### 3. BlobStatus::Partial Size Field

**Issue**: The `size` field in `BlobStatus::Partial` is `Option<u64>`, not `u64`.

**Workaround**:
```rust
match status {
    BlobStatus::Complete { size } => Ok(size),
    BlobStatus::Partial { size, .. } => Ok(size.unwrap_or(0)),
    BlobStatus::NotFound => Err(Error::NotFound),
}
```

### 4. Hash Parsing Can Panic

**Issue**: Parsing invalid strings as `iroh_blobs::Hash` can panic in the underlying data-encoding library, rather than returning a Result.

**Workaround**: Validate hash format before parsing:
```rust
fn is_valid_hash(s: &str) -> bool {
    // BLAKE3 hashes are 64 hex characters
    s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit())
}
```

### 5. Automerge Methods Require Mutable Access

**Issue**: Several Automerge methods like `get_heads()` and sync-related methods require mutable access even though they seem like read operations.

**Workaround**: Use write lock instead of read lock:
```rust
// Incorrect
let doc = self.doc.read();
doc.get_heads()  // Error: needs mutable borrow

// Correct
let mut doc = self.doc.write();
doc.get_heads()
```

## Testing Recommendations

1. **Use RelayMode::Disabled for unit/integration tests** - Avoids network dependencies and timeouts.

2. **Mark n0-dependent tests as #[ignore]** - Tests that require `discovery_n0()` should be marked as ignored and run separately.

3. **Use explicit NodeAddr for testing** - Manually construct `NodeAddr` from direct addresses instead of relying on discovery.

4. **Set reasonable timeouts** - Wrap async operations in `tokio::time::timeout()` to catch hangs.

## Performance Notes

- Endpoint creation with `discovery_n0()`: ~5-30 seconds
- Endpoint creation with `RelayMode::Disabled`: ~50ms
- Connection establishment (local): ~10-50ms
- iroh-blobs FsStore initialization: ~100ms

## Migration from Earlier Versions

If upgrading from pre-0.92 iroh:

1. `node_addr()` now returns a Watcher, not a future directly
2. Use `endpoint.node_addr().initialized().await` pattern
3. Check iroh-blobs API for field vs method changes
4. Review async/sync method signatures - some changed
