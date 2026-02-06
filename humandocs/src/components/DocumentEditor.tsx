import { useState, useEffect, useRef, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import type { DocContent, CursorInfo } from '../types';
import Attachments from './Attachments';
import { getCaretCoordinates } from '../utils/caretCoordinates';

interface DocumentEditorProps {
  doc: DocContent;
  lastSync: string | null;
  onSave: (docId: string, title: string, content: string) => Promise<void>;
  onAddAttachment: (docId: string, filePath: string) => void;
  onOpenAttachment: (blobHash: string) => void;
}

export default function DocumentEditor({
  doc,
  lastSync,
  onSave,
  onAddAttachment,
  onOpenAttachment,
}: DocumentEditorProps) {
  const [title, setTitle] = useState(doc.title);
  const [content, setContent] = useState(doc.content);
  const [saveStatus, setSaveStatus] = useState<'saved' | 'saving' | 'unsaved'>('saved');
  const timerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const docIdRef = useRef(doc.id);
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const lastSyncRef = useRef<string | null>(null);
  const [cursors, setCursors] = useState<CursorInfo[]>([]);
  const deviceIdRef = useRef<string>('');
  const [_scrollTop, setScrollTop] = useState(0);
  const [, setResizeKey] = useState(0);

  // Get device ID on mount
  useEffect(() => {
    invoke<string>('get_device_id')
      .then((id) => { deviceIdRef.current = id; })
      .catch(() => {});
  }, []);

  useEffect(() => {
    if (doc.id !== docIdRef.current) {
      docIdRef.current = doc.id;
      setTitle(doc.title);
      setContent(doc.content);
      setSaveStatus('saved');
      if (timerRef.current) clearTimeout(timerRef.current);
    }
  }, [doc.id, doc.title, doc.content]);

  // Re-fetch doc content when sync happens and user isn't actively editing
  useEffect(() => {
    if (!lastSync || lastSync === lastSyncRef.current) return;
    lastSyncRef.current = lastSync;
    if (saveStatus !== 'saved') return; // don't clobber unsaved edits

    (async () => {
      try {
        const fresh = await invoke<DocContent>('open_document', { docId: doc.id });
        if (fresh.content !== content || fresh.title !== title) {
          setTitle(fresh.title);
          setContent(fresh.content);
        }
      } catch (_) {
        // ignore
      }
    })();
  }, [lastSync, doc.id, saveStatus, content, title]);

  // Auto-grow
  useEffect(() => {
    if (textareaRef.current) {
      textareaRef.current.style.height = 'auto';
      textareaRef.current.style.height = textareaRef.current.scrollHeight + 'px';
    }
  }, [content]);

  // Recalculate cursor positions on window resize
  useEffect(() => {
    const handleResize = () => setResizeKey((k) => k + 1);
    window.addEventListener('resize', handleResize);
    return () => window.removeEventListener('resize', handleResize);
  }, []);

  // Report cursor position to backend
  useEffect(() => {
    const textarea = textareaRef.current;
    if (!textarea) return;

    let debounceTimer: ReturnType<typeof setTimeout> | null = null;
    const reportCursor = (activity: string) => {
      if (debounceTimer) clearTimeout(debounceTimer);
      debounceTimer = setTimeout(() => {
        const pos = textarea.selectionStart ?? 0;
        invoke('update_cursor', {
          docId: doc.id,
          position: pos,
          deviceName: deviceIdRef.current || 'Device',
          activity,
        }).catch(() => {});
      }, 500);
    };

    const handleClick = () => reportCursor('viewing');
    const handleKeyup = () => reportCursor('typing');

    textarea.addEventListener('click', handleClick);
    textarea.addEventListener('keyup', handleKeyup);

    return () => {
      if (debounceTimer) clearTimeout(debounceTimer);
      textarea.removeEventListener('click', handleClick);
      textarea.removeEventListener('keyup', handleKeyup);
    };
  }, [doc.id]);

  // Poll remote cursors
  useEffect(() => {
    const poll = () => {
      invoke<CursorInfo[]>('get_cursors', { docId: doc.id })
        .then(setCursors)
        .catch(() => {});
    };
    poll();
    const interval = setInterval(poll, 500);
    return () => clearInterval(interval);
  }, [doc.id]);

  // Update toolbar status text
  useEffect(() => {
    const el = document.getElementById('save-status');
    if (el) {
      el.textContent = saveStatus === 'saving' ? 'Saving...' : saveStatus === 'unsaved' ? 'Edited' : 'Saved';
      el.className = `toolbar-status ${saveStatus === 'saving' ? 'saving' : ''}`;
    }
  }, [saveStatus]);

  // Wire the attachment button in the toolbar
  useEffect(() => {
    const btn = document.getElementById('attach-btn');
    if (!btn) return;
    const fileInput = document.createElement('input');
    fileInput.type = 'file';
    fileInput.style.display = 'none';
    document.body.appendChild(fileInput);

    const handleClick = () => fileInput.click();
    const handleChange = () => {
      const file = fileInput.files?.[0];
      if (file) {
        const filePath = (file as unknown as { path?: string }).path ?? file.name;
        onAddAttachment(doc.id, filePath);
      }
      fileInput.value = '';
    };

    btn.addEventListener('click', handleClick);
    fileInput.addEventListener('change', handleChange);

    return () => {
      btn.removeEventListener('click', handleClick);
      fileInput.removeEventListener('change', handleChange);
      document.body.removeChild(fileInput);
    };
  }, [doc.id, onAddAttachment]);

  const debouncedSave = useCallback(
    (newTitle: string, newContent: string) => {
      setSaveStatus('unsaved');
      if (timerRef.current) clearTimeout(timerRef.current);
      timerRef.current = setTimeout(async () => {
        setSaveStatus('saving');
        await onSave(doc.id, newTitle, newContent);
        setSaveStatus('saved');
      }, 300);
    },
    [doc.id, onSave],
  );

  useEffect(() => {
    return () => { if (timerRef.current) clearTimeout(timerRef.current); };
  }, []);

  const handleScroll = () => {
    if (textareaRef.current) {
      setScrollTop(textareaRef.current.scrollTop);
    }
  };

  return (
    <div className="editor">
      <div className="editor-scroll">
        <input
          className="editor-title"
          type="text"
          value={title}
          onChange={(e) => { setTitle(e.target.value); debouncedSave(e.target.value, content); }}
          placeholder="Title"
        />

        <div className="editor-textarea-wrap">
          <textarea
            ref={textareaRef}
            className="editor-content"
            value={content}
            onChange={(e) => { setContent(e.target.value); debouncedSave(title, e.target.value); }}
            onScroll={handleScroll}
            placeholder="Start writing..."
          />

          {cursors.map((c) => {
            const ta = textareaRef.current;
            if (!ta) return null;
            const coords = getCaretCoordinates(ta, c.position);
            return (
              <div
                key={c.device_id}
                className="remote-cursor"
                style={{ left: coords.left, top: coords.top }}
              >
                <div
                  className="remote-cursor-flag"
                  style={{ backgroundColor: c.color }}
                >
                  {c.device_name}
                </div>
                <div
                  className="remote-cursor-line"
                  style={{ backgroundColor: c.color, height: coords.height }}
                />
              </div>
            );
          })}
        </div>

        {doc.attachments.length > 0 && (
          <Attachments
            attachments={doc.attachments}
            onAdd={(filePath) => onAddAttachment(doc.id, filePath)}
            onOpen={onOpenAttachment}
          />
        )}
      </div>
    </div>
  );
}
