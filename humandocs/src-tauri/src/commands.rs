use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use uuid::Uuid;

use humansync::{Config, HumanSync, SyncStatus};

use crate::AppState;

/// Prefix for all Humandocs documents in the HumanSync store.
const DOC_PREFIX: &str = "humandocs/doc-";

/// App configuration saved to disk so we can restore on next launch.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct AppConfig {
    server_url: String,
    storage_path: String,
    /// The server's Iroh NodeId, so we can re-add it to the registry on restart.
    #[serde(default)]
    server_node_id: Option<String>,
}

/// A document summary returned by `list_documents`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocSummary {
    pub id: String,
    pub title: String,
    pub created_at: String,
    pub updated_at: String,
}

/// Full document content returned by `open_document`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocContent {
    pub id: String,
    pub title: String,
    pub content: String,
    pub created_at: String,
    pub updated_at: String,
    pub attachments: Vec<AttachmentInfo>,
}

/// Attachment metadata stored inside a document.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttachmentInfo {
    pub name: String,
    pub blob_hash: String,
}

/// Sync status returned to the frontend.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncInfo {
    pub peers_connected: usize,
    pub docs_pending: usize,
    pub last_sync: Option<String>,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn config_path(data_dir: &Path) -> PathBuf {
    data_dir.join("humandocs_config.json")
}

fn storage_path(data_dir: &Path) -> PathBuf {
    data_dir.join("humansync_data")
}

async fn load_saved_config(data_dir: &Path) -> Option<AppConfig> {
    let path = config_path(data_dir);
    let bytes = tokio::fs::read(&path).await.ok()?;
    serde_json::from_slice(&bytes).ok()
}

async fn save_config(data_dir: &Path, cfg: &AppConfig) -> Result<(), String> {
    let path = config_path(data_dir);
    let bytes = serde_json::to_vec_pretty(cfg).map_err(|e| e.to_string())?;
    tokio::fs::write(&path, bytes)
        .await
        .map_err(|e| e.to_string())
}

async fn ensure_node(
    node_lock: &Mutex<Option<Arc<HumanSync>>>,
) -> Result<Arc<HumanSync>, String> {
    let guard = node_lock.lock().await;
    guard
        .clone()
        .ok_or_else(|| "App is not initialized. Call init_app first.".to_string())
}

fn sync_status_to_info(s: &SyncStatus) -> SyncInfo {
    SyncInfo {
        peers_connected: s.peers_connected,
        docs_pending: s.docs_pending,
        last_sync: s.last_sync.map(|dt| dt.to_rfc3339()),
    }
}

// ---------------------------------------------------------------------------
// Tauri commands
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn init_app(
    state: tauri::State<'_, AppState>,
    server_url: String,
    password: String,
) -> Result<(), String> {
    let storage = storage_path(&state.data_dir);

    let config = Config::new(&storage).with_server_url(&server_url);

    let node = HumanSync::init(config)
        .await
        .map_err(|e| format!("Failed to initialize HumanSync: {e}"))?;

    node.pair(&server_url, &password)
        .await
        .map_err(|e| format!("Pairing failed: {e}"))?;

    // Grab the server's NodeId from the registry so we can persist it
    let my_node_id = node.node_id().ok();
    let server_node_id = node
        .registry()
        .list()
        .iter()
        .find(|d| Some(d.node_id) != my_node_id)
        .map(|d| d.node_id.to_string());

    let app_cfg = AppConfig {
        server_url,
        storage_path: storage.to_string_lossy().into_owned(),
        server_node_id,
    };
    save_config(&state.data_dir, &app_cfg).await?;

    let mut guard = state.node.lock().await;
    *guard = Some(Arc::new(node));

    Ok(())
}

#[tauri::command]
pub async fn is_paired(state: tauri::State<'_, AppState>) -> Result<bool, String> {
    // Hold the lock for the entire operation to prevent double-init
    // (React 18 strict mode calls useEffect twice in dev).
    let mut guard = state.node.lock().await;

    if guard.is_some() {
        return Ok(true);
    }

    let Some(app_cfg) = load_saved_config(&state.data_dir).await else {
        return Ok(false);
    };

    let config = Config::new(&app_cfg.storage_path).with_server_url(&app_cfg.server_url);

    match HumanSync::init(config).await {
        Ok(node) => {
            // Restore the server peer into the registry so sync works after restart
            if let Some(ref server_nid) = app_cfg.server_node_id {
                if let Ok(nid) = server_nid.parse::<humansync::NodeId>() {
                    node.registry()
                        .add(humansync::registry::DeviceInfo::new(nid, "Server"));
                }
            }
            *guard = Some(Arc::new(node));
            Ok(true)
        }
        Err(e) => {
            tracing::warn!("Failed to restore node from saved config: {e}");
            Ok(false)
        }
    }
}

#[tauri::command]
pub async fn list_documents(
    state: tauri::State<'_, AppState>,
) -> Result<Vec<DocSummary>, String> {
    let node = ensure_node(&state.node).await?;

    let names = node
        .list_docs(DOC_PREFIX)
        .map_err(|e| format!("Failed to list documents: {e}"))?;

    let mut docs = Vec::with_capacity(names.len());
    for name in names {
        let id = name
            .strip_prefix(DOC_PREFIX)
            .unwrap_or(&name)
            .to_string();

        if let Ok(doc) = node.open_doc(&name).await {
            // Skip soft-deleted docs.
            let deleted: Option<bool> = doc.get("deleted").ok().flatten();
            if deleted == Some(true) {
                continue;
            }

            let title: String = doc
                .get("title")
                .ok()
                .flatten()
                .unwrap_or_else(|| "Untitled".to_string());
            let created_at: String = doc.get("created_at").ok().flatten().unwrap_or_default();
            let updated_at: String = doc.get("updated_at").ok().flatten().unwrap_or_default();

            docs.push(DocSummary {
                id,
                title,
                created_at,
                updated_at,
            });
        }
    }

    Ok(docs)
}

#[tauri::command]
pub async fn create_document(
    state: tauri::State<'_, AppState>,
    title: String,
) -> Result<DocSummary, String> {
    let node = ensure_node(&state.node).await?;

    let id = Uuid::new_v4().to_string();
    let doc_name = format!("{DOC_PREFIX}{id}");
    let now = Utc::now().to_rfc3339();

    let doc = node
        .open_doc(&doc_name)
        .await
        .map_err(|e| format!("Failed to create document: {e}"))?;

    doc.put("title", &title).map_err(|e| e.to_string())?;
    doc.put("content", "").map_err(|e| e.to_string())?;
    doc.ensure_text("content").map_err(|e| e.to_string())?;
    doc.put("created_at", &now).map_err(|e| e.to_string())?;
    doc.put("updated_at", &now).map_err(|e| e.to_string())?;
    doc.put("attachments", "[]").map_err(|e| e.to_string())?;
    doc.save().map_err(|e| e.to_string())?;

    Ok(DocSummary {
        id,
        title,
        created_at: now.clone(),
        updated_at: now,
    })
}

#[tauri::command]
pub async fn open_document(
    state: tauri::State<'_, AppState>,
    doc_id: String,
) -> Result<DocContent, String> {
    let node = ensure_node(&state.node).await?;

    let doc_name = format!("{DOC_PREFIX}{doc_id}");
    let doc = node
        .open_doc(&doc_name)
        .await
        .map_err(|e| format!("Failed to open document: {e}"))?;

    let title: String = doc
        .get("title")
        .ok()
        .flatten()
        .unwrap_or_else(|| "Untitled".to_string());
    let content: String = doc.get_text("content").map_err(|e| format!("Failed to read content: {e}"))?.unwrap_or_default();
    let created_at: String = doc.get("created_at").ok().flatten().unwrap_or_default();
    let updated_at: String = doc.get("updated_at").ok().flatten().unwrap_or_default();
    let attachments_json: String = doc
        .get("attachments")
        .ok()
        .flatten()
        .unwrap_or_else(|| "[]".to_string());

    let attachments: Vec<AttachmentInfo> =
        serde_json::from_str(&attachments_json).unwrap_or_default();

    Ok(DocContent {
        id: doc_id,
        title,
        content,
        created_at,
        updated_at,
        attachments,
    })
}

#[tauri::command]
pub async fn save_document(
    state: tauri::State<'_, AppState>,
    doc_id: String,
    title: String,
    content: String,
) -> Result<(), String> {
    let node = ensure_node(&state.node).await?;

    let doc_name = format!("{DOC_PREFIX}{doc_id}");
    let doc = node
        .open_doc(&doc_name)
        .await
        .map_err(|e| format!("Failed to open document: {e}"))?;

    let now = Utc::now().to_rfc3339();
    doc.put("title", &title).map_err(|e| e.to_string())?;
    doc.update_text("content", &content).map_err(|e| e.to_string())?;
    doc.put("updated_at", &now).map_err(|e| e.to_string())?;
    doc.save().map_err(|e| e.to_string())?;

    // Fire-and-forget sync so changes reach other devices immediately
    let node_clone = node.clone();
    tokio::spawn(async move {
        if let Err(e) = node_clone.sync_now().await {
            tracing::warn!(error = %e, "Sync after save failed");
        }
    });

    Ok(())
}

#[tauri::command]
pub async fn delete_document(
    state: tauri::State<'_, AppState>,
    doc_id: String,
) -> Result<(), String> {
    let node = ensure_node(&state.node).await?;

    let doc_name = format!("{DOC_PREFIX}{doc_id}");
    let doc = node
        .open_doc(&doc_name)
        .await
        .map_err(|e| format!("Failed to open document for deletion: {e}"))?;

    doc.put("title", "[deleted]").map_err(|e| e.to_string())?;
    doc.put("content", "").map_err(|e| e.to_string())?;
    doc.put("deleted", true).map_err(|e| e.to_string())?;
    doc.put("updated_at", &Utc::now().to_rfc3339())
        .map_err(|e| e.to_string())?;
    doc.save().map_err(|e| e.to_string())?;

    Ok(())
}

#[tauri::command]
pub async fn add_attachment(
    state: tauri::State<'_, AppState>,
    doc_id: String,
    file_path: String,
) -> Result<AttachmentInfo, String> {
    let node = ensure_node(&state.node).await?;

    let path = Path::new(&file_path);
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("attachment")
        .to_string();

    let blob_hash = node
        .store_blob(path)
        .await
        .map_err(|e| format!("Failed to store blob: {e}"))?;

    let doc_name = format!("{DOC_PREFIX}{doc_id}");
    let doc = node
        .open_doc(&doc_name)
        .await
        .map_err(|e| format!("Failed to open document: {e}"))?;

    let attachments_json: String = doc
        .get("attachments")
        .ok()
        .flatten()
        .unwrap_or_else(|| "[]".to_string());

    let mut attachments: Vec<AttachmentInfo> =
        serde_json::from_str(&attachments_json).unwrap_or_default();

    let info = AttachmentInfo {
        name: file_name,
        blob_hash: blob_hash.clone(),
    };
    attachments.push(info.clone());

    let new_json = serde_json::to_string(&attachments).map_err(|e| e.to_string())?;
    doc.put("attachments", &new_json)
        .map_err(|e| e.to_string())?;
    doc.put("updated_at", &Utc::now().to_rfc3339())
        .map_err(|e| e.to_string())?;
    doc.save().map_err(|e| e.to_string())?;

    Ok(info)
}

#[tauri::command]
pub async fn get_attachment(
    state: tauri::State<'_, AppState>,
    blob_hash: String,
) -> Result<String, String> {
    let node = ensure_node(&state.node).await?;

    let path = node
        .get_blob(&blob_hash)
        .await
        .map_err(|e| format!("Failed to get blob: {e}"))?;

    Ok(path.to_string_lossy().into_owned())
}

#[tauri::command]
pub async fn get_sync_status(
    state: tauri::State<'_, AppState>,
) -> Result<SyncInfo, String> {
    let node = ensure_node(&state.node).await?;
    Ok(sync_status_to_info(&node.sync_status()))
}

#[tauri::command]
pub async fn sync_now(state: tauri::State<'_, AppState>) -> Result<SyncInfo, String> {
    let node = ensure_node(&state.node).await?;

    node.sync_now()
        .await
        .map_err(|e| format!("Sync failed: {e}"))?;

    Ok(sync_status_to_info(&node.sync_status()))
}

#[tauri::command]
pub async fn update_cursor(
    state: tauri::State<'_, AppState>,
    doc_id: String,
    position: u64,
    device_name: String,
    activity: String,
) -> Result<(), String> {
    let node = ensure_node(&state.node).await?;
    let full_id = node.node_id().map_err(|e| format!("No node ID: {e}"))?.to_string();
    let device_id = &full_id[..8.min(full_id.len())];
    let doc_name = format!("{DOC_PREFIX}{doc_id}");
    let doc = node
        .open_doc(&doc_name)
        .await
        .map_err(|e| format!("Failed to open document: {e}"))?;
    doc.set_cursor(device_id, position, &device_name, &activity)
        .map_err(|e| e.to_string())?;
    doc.save().map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn get_cursors(
    state: tauri::State<'_, AppState>,
    doc_id: String,
) -> Result<Vec<humansync::doc::CursorInfo>, String> {
    let node = ensure_node(&state.node).await?;
    let full_id = node.node_id().map_err(|e| format!("No node ID: {e}"))?.to_string();
    let device_id = &full_id[..8.min(full_id.len())];
    let doc_name = format!("{DOC_PREFIX}{doc_id}");
    let doc = node
        .open_doc(&doc_name)
        .await
        .map_err(|e| format!("Failed to open document: {e}"))?;
    doc.get_cursors(device_id).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn get_device_id(
    state: tauri::State<'_, AppState>,
) -> Result<String, String> {
    let node = ensure_node(&state.node).await?;
    let full_id = node.node_id().map_err(|e| format!("No node ID: {e}"))?.to_string();
    Ok(full_id[..8.min(full_id.len())].to_string())
}
