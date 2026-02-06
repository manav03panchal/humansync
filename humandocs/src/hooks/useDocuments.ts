import { useState, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import type { DocSummary, DocContent, AttachmentInfo } from '../types';

export function useDocuments() {
  const [documents, setDocuments] = useState<DocSummary[]>([]);
  const [currentDoc, setCurrentDoc] = useState<DocContent | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const clearError = useCallback(() => setError(null), []);

  const loadDocuments = useCallback(async () => {
    try {
      const docs = await invoke<DocSummary[]>('list_documents');
      setDocuments(docs.sort((a, b) =>
        new Date(b.updated_at).getTime() - new Date(a.updated_at).getTime()
      ));
    } catch (e) {
      setError(String(e));
    }
  }, []);

  const createDocument = useCallback(async (title: string) => {
    try {
      setLoading(true);
      const doc = await invoke<DocSummary>('create_document', { title });
      await loadDocuments();
      return doc;
    } catch (e) {
      setError(String(e));
      return null;
    } finally {
      setLoading(false);
    }
  }, [loadDocuments]);

  const openDocument = useCallback(async (docId: string) => {
    try {
      setLoading(true);
      const doc = await invoke<DocContent>('open_document', { docId });
      setCurrentDoc(doc);
      return doc;
    } catch (e) {
      setError(String(e));
      return null;
    } finally {
      setLoading(false);
    }
  }, []);

  const saveDocument = useCallback(async (docId: string, title: string, content: string) => {
    try {
      await invoke('save_document', { docId, title, content });
      await loadDocuments();
    } catch (e) {
      setError(String(e));
    }
  }, [loadDocuments]);

  const deleteDocument = useCallback(async (docId: string) => {
    try {
      setLoading(true);
      await invoke('delete_document', { docId });
      if (currentDoc?.id === docId) {
        setCurrentDoc(null);
      }
      await loadDocuments();
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }, [currentDoc, loadDocuments]);

  const addAttachment = useCallback(async (docId: string, filePath: string) => {
    try {
      const info = await invoke<AttachmentInfo>('add_attachment', { docId, filePath });
      // Reload the current doc to refresh attachments
      if (currentDoc?.id === docId) {
        await openDocument(docId);
      }
      return info;
    } catch (e) {
      setError(String(e));
      return null;
    }
  }, [currentDoc, openDocument]);

  const openAttachment = useCallback(async (blobHash: string) => {
    try {
      const path = await invoke<string>('get_attachment', { blobHash });
      // The backend returns a file path; we can't open it from the web context directly
      // but the invoke itself may trigger the OS to open the file
      return path;
    } catch (e) {
      setError(String(e));
      return null;
    }
  }, []);

  const closeDocument = useCallback(() => {
    setCurrentDoc(null);
  }, []);

  return {
    documents,
    currentDoc,
    loading,
    error,
    clearError,
    loadDocuments,
    createDocument,
    openDocument,
    saveDocument,
    deleteDocument,
    addAttachment,
    openAttachment,
    closeDocument,
  };
}
