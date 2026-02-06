export interface DocSummary {
  id: string;
  title: string;
  created_at: string;
  updated_at: string;
}

export interface DocContent {
  id: string;
  title: string;
  content: string;
  created_at: string;
  updated_at: string;
  attachments: AttachmentInfo[];
}

export interface AttachmentInfo {
  name: string;
  blob_hash: string;
}

export interface SyncInfo {
  peers_connected: number;
  docs_pending: number;
  last_sync: string | null;
}

export interface CursorInfo {
  device_id: string;
  device_name: string;
  position: number;
  color: string;
  activity: string; // "typing" | "viewing"
}
