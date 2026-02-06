import type { SyncInfo } from '../types';

interface SyncStatusProps {
  syncInfo: SyncInfo;
  syncing: boolean;
  onSync: () => void;
}

function fmtSync(lastSync: string | null): string {
  if (!lastSync) return 'Never';
  const diff = Date.now() - new Date(lastSync).getTime();
  const s = Math.floor(diff / 1000);
  if (s < 60) return 'Just now';
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m ago`;
  const h = Math.floor(m / 60);
  if (h < 24) return `${h}h ago`;
  return `${Math.floor(h / 24)}d ago`;
}

export default function SyncStatus({ syncInfo, syncing, onSync }: SyncStatusProps) {
  const recent = syncInfo.last_sync
    ? (Date.now() - new Date(syncInfo.last_sync).getTime()) < 90_000
    : false;
  const connected = syncInfo.peers_connected > 0 || recent;
  const dotClass = syncing ? 'busy' : connected ? 'on' : 'off';
  const label = syncing ? 'Syncing' : connected
    ? (syncInfo.peers_connected > 0 ? `${syncInfo.peers_connected}` : 'Synced')
    : 'Offline';

  return (
    <div className="sync-wrap">
      <button className="sync-btn" onClick={onSync} title="Sync">
        <span className={`sync-dot ${dotClass}`} />
        <span>{label}</span>
      </button>
      <div className="sync-tip">
        <div className="sync-row"><span>Peers</span><span className="sync-val">{syncInfo.peers_connected}</span></div>
        <div className="sync-row"><span>Pending</span><span className="sync-val">{syncInfo.docs_pending}</span></div>
        <div className="sync-row"><span>Last sync</span><span className="sync-val">{fmtSync(syncInfo.last_sync)}</span></div>
      </div>
    </div>
  );
}
