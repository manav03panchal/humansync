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

const svgProps = { width: 14, height: 14, viewBox: '0 0 24 24', fill: 'none', stroke: 'currentColor', strokeWidth: 2, strokeLinecap: 'round' as const, strokeLinejoin: 'round' as const };

function getIcon(name: string): JSX.Element {
  const ext = name.split('.').pop()?.toLowerCase() ?? '';
  if (['jpg', 'jpeg', 'png', 'gif', 'svg', 'webp'].includes(ext))
    return <svg {...svgProps}><rect x="3" y="3" width="18" height="18" rx="2" /><circle cx="8.5" cy="8.5" r="1.5" /><polyline points="21 15 16 10 5 21" /></svg>;
  if (['pdf'].includes(ext))
    return <svg {...svgProps}><path d="M14 2H6a2 2 0 00-2 2v16a2 2 0 002 2h12a2 2 0 002-2V8z" /><polyline points="14 2 14 8 20 8" /></svg>;
  if (['zip', 'tar', 'gz', 'rar'].includes(ext))
    return <svg {...svgProps}><path d="M21 16V8a2 2 0 00-1-1.73l-7-4a2 2 0 00-2 0l-7 4A2 2 0 003 8v8a2 2 0 001 1.73l7 4a2 2 0 002 0l7-4A2 2 0 0021 16z" /></svg>;
  return <svg {...svgProps}><path d="M21.44 11.05l-9.19 9.19a6 6 0 01-8.49-8.49l9.19-9.19a4 4 0 015.66 5.66l-9.2 9.19a2 2 0 01-2.83-2.83l8.49-8.48" /></svg>;
}
