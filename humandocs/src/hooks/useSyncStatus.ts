import { useState, useEffect, useCallback, useRef } from 'react';
import { invoke } from '@tauri-apps/api/core';
import type { SyncInfo } from '../types';

export function useSyncStatus() {
  const [syncInfo, setSyncInfo] = useState<SyncInfo>({
    peers_connected: 0,
    docs_pending: 0,
    last_sync: null,
  });
  const [syncing, setSyncing] = useState(false);
  const intervalRef = useRef<ReturnType<typeof setInterval> | null>(null);

  const fetchStatus = useCallback(async () => {
    try {
      const info = await invoke<SyncInfo>('get_sync_status');
      setSyncInfo(info);
    } catch (_) {
      // Silently ignore polling errors
    }
  }, []);

  const triggerSync = useCallback(async () => {
    try {
      setSyncing(true);
      const info = await invoke<SyncInfo>('sync_now');
      setSyncInfo(info);
    } catch (_) {
      // Ignore sync errors silently
    } finally {
      setSyncing(false);
    }
  }, []);

  useEffect(() => {
    fetchStatus();
    intervalRef.current = setInterval(fetchStatus, 1000);
    return () => {
      if (intervalRef.current) {
        clearInterval(intervalRef.current);
      }
    };
  }, [fetchStatus]);

  return { syncInfo, syncing, triggerSync };
}
