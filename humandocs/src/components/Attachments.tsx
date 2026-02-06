import type { AttachmentInfo } from '../types';

interface AttachmentsProps {
  attachments: AttachmentInfo[];
  onAdd: (filePath: string) => void;
  onOpen: (blobHash: string) => void;
}

export default function Attachments({ attachments, onOpen }: AttachmentsProps) {
  return (
    <div className="att-section">
      <div className="att-header">
        <span className="att-label">Attachments</span>
      </div>
      <div className="att-list">
        {attachments.map((att) => (
          <button key={att.blob_hash} className="att-chip" onClick={() => onOpen(att.blob_hash)}>
            <span className="att-icon">{getIcon(att.name)}</span>
            <span>{att.name}</span>
          </button>
        ))}
      </div>
    </div>
  );
}

function getIcon(name: string): string {
  const ext = name.split('.').pop()?.toLowerCase() ?? '';
  if (['jpg', 'jpeg', 'png', 'gif', 'svg', 'webp'].includes(ext)) return '\u{1F5BC}';
  if (['pdf'].includes(ext)) return '\u{1F4C4}';
  if (['zip', 'tar', 'gz', 'rar'].includes(ext)) return '\u{1F4E6}';
  return '\u{1F4CE}';
}
