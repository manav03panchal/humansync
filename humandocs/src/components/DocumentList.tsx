import { useState } from 'react';
import type { DocSummary } from '../types';

interface DocumentListProps {
  documents: DocSummary[];
  activeDocId: string | null;
  onSelect: (docId: string) => void;
}

function timeAgo(dateStr: string): string {
  const diff = Date.now() - new Date(dateStr).getTime();
  const seconds = Math.floor(diff / 1000);
  if (seconds < 60) return 'Just now';
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours}h`;
  const days = Math.floor(hours / 24);
  if (days < 7) return `${days}d`;
  return new Date(dateStr).toLocaleDateString(undefined, { month: 'short', day: 'numeric' });
}

export default function DocumentList({ documents, activeDocId, onSelect }: DocumentListProps) {
  const [search, setSearch] = useState('');

  const filtered = search.trim()
    ? documents.filter((d) =>
        d.title.toLowerCase().includes(search.toLowerCase())
      )
    : documents;

  return (
    <div className="doc-list-container">
      <div className="search-container">
        <input
          className="search-input"
          type="text"
          placeholder="Search"
          value={search}
          onChange={(e) => setSearch(e.target.value)}
        />
      </div>

      <div className="doc-list">
        {filtered.length > 0 ? (
          filtered.map((doc) => (
            <div
              key={doc.id}
              className={`doc-item ${doc.id === activeDocId ? 'active' : ''}`}
              onClick={() => onSelect(doc.id)}
            >
              <div className="doc-title">{doc.title || 'Untitled'}</div>
              <div className="doc-meta">
                <span className="doc-time">{timeAgo(doc.updated_at)}</span>
              </div>
            </div>
          ))
        ) : (
          <div className="doc-list-empty">
            {search.trim() ? 'No results' : 'No notes yet'}
          </div>
        )}
      </div>
    </div>
  );
}
