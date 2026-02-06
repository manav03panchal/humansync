import { useState, useEffect, useCallback, useRef } from 'react';

import { useDocuments } from '../hooks/useDocuments';
import { useSyncStatus } from '../hooks/useSyncStatus';
import DocumentList from './DocumentList';
import DocumentEditor from './DocumentEditor';
import SyncStatus from './SyncStatus';

export default function AppLayout() {
  const {
    documents,
    currentDoc,
    error,
    clearError,
    loadDocuments,
    createDocument,
    openDocument,
    saveDocument,
    deleteDocument,
    addAttachment,
    openAttachment,
  } = useDocuments();

  const { syncInfo, syncing, triggerSync } = useSyncStatus();
  const [sidebarOpen, setSidebarOpen] = useState(true);
  const [showDelete, setShowDelete] = useState(false);
  const lastSyncRef = useRef<string | null>(null);

  useEffect(() => {
    loadDocuments();
  }, [loadDocuments]);

  // Auto-refresh doc list + open doc when sync happens
  useEffect(() => {
    if (syncInfo.last_sync && syncInfo.last_sync !== lastSyncRef.current) {
      lastSyncRef.current = syncInfo.last_sync;
      loadDocuments();
    }
  }, [syncInfo.last_sync, loadDocuments]);

  const handleNewDoc = useCallback(async () => {
    const doc = await createDocument('Untitled');
    if (doc) {
      await openDocument(doc.id);
    }
  }, [createDocument, openDocument]);

  const handleDelete = useCallback(async () => {
    if (currentDoc) {
      setShowDelete(false);
      await deleteDocument(currentDoc.id);
    }
  }, [currentDoc, deleteDocument]);

  return (
    <div className="app-layout">
      <aside className={`sidebar ${sidebarOpen ? '' : 'collapsed'}`}>
        <div className="sidebar-top">
          <span className="sidebar-title">Notes</span>
          <div className="sidebar-btns">
            <SyncStatus syncInfo={syncInfo} syncing={syncing} onSync={triggerSync} />
            <button className="tb" onClick={handleNewDoc} title="New note">
              <svg viewBox="0 0 24 24"><path d="M12 5v14M5 12h14" /></svg>
            </button>
          </div>
        </div>

        <DocumentList
          documents={documents}
          activeDocId={currentDoc?.id ?? null}
          onSelect={(id) => openDocument(id)}
        />
      </aside>

      <div className="main-area">
        <div className="editor-toolbar">
          <div className="toolbar-left">
            <button
              className="tb muted"
              onClick={() => setSidebarOpen(!sidebarOpen)}
              title={sidebarOpen ? 'Hide sidebar' : 'Show sidebar'}
            >
              <svg viewBox="0 0 24 24">
                <rect x="3" y="3" width="18" height="18" rx="3" />
                <path d="M9 3v18" />
              </svg>
            </button>
          </div>

          <div className="toolbar-center">
            {error ? (
              <span className="toolbar-status" style={{ color: 'var(--red)', cursor: 'pointer' }} onClick={clearError}>
                {error} (dismiss)
              </span>
            ) : currentDoc ? (
              <span className={`toolbar-status ${currentDoc ? '' : ''}`} id="save-status">
                Saved
              </span>
            ) : null}
          </div>

          <div className="toolbar-right">
            {currentDoc && (
              <>
                <button className="tb" title="Add attachment" id="attach-btn">
                  <svg viewBox="0 0 24 24"><path d="M21.44 11.05l-9.19 9.19a6 6 0 01-8.49-8.49l9.19-9.19a4 4 0 015.66 5.66l-9.2 9.19a2 2 0 01-2.83-2.83l8.49-8.48" /></svg>
                </button>
                <button className="tb danger" onClick={() => setShowDelete(true)} title="Delete note">
                  <svg viewBox="0 0 24 24"><path d="M3 6h18M8 6V4a2 2 0 012-2h4a2 2 0 012 2v2M19 6l-1 14a2 2 0 01-2 2H8a2 2 0 01-2-2L5 6" /><path d="M10 11v6M14 11v6" /></svg>
                </button>
              </>
            )}
          </div>
        </div>

        {currentDoc ? (
          <DocumentEditor
            key={currentDoc.id}
            doc={currentDoc}
            lastSync={syncInfo.last_sync}
            onSave={saveDocument}
            onAddAttachment={addAttachment}
            onOpenAttachment={openAttachment}
          />
        ) : (
          <div className="empty-state">
            <div className="empty-icon">{'\u{1F4DD}'}</div>
            <div className="empty-title">No note selected</div>
            <div className="empty-text">Select a note or create a new one</div>
          </div>
        )}
      </div>

      {showDelete && currentDoc && (
        <div className="dialog-bg" onClick={() => setShowDelete(false)}>
          <div className="dialog" onClick={(e) => e.stopPropagation()}>
            <div className="dialog-body">
              <div className="dialog-title">Delete Note</div>
              <div className="dialog-msg">
                Are you sure you want to delete "{currentDoc.title || 'Untitled'}"?
              </div>
            </div>
            <div className="dialog-btns">
              <button className="dialog-btn destructive" onClick={handleDelete}>Delete</button>
              <button className="dialog-btn cancel" onClick={() => setShowDelete(false)}>Cancel</button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
