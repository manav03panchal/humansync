import { useState } from 'react';
import { invoke } from '@tauri-apps/api/core';

interface SetupScreenProps {
  onConnected: () => void;
}

export default function SetupScreen({ onConnected }: SetupScreenProps) {
  const [serverUrl, setServerUrl] = useState('');
  const [password, setPassword] = useState('');
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!serverUrl.trim() || !password.trim()) return;

    setLoading(true);
    setError(null);

    try {
      await invoke('init_app', { serverUrl: serverUrl.trim(), password: password.trim() });
      onConnected();
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  };

  return (
    <div className="setup-screen">
      <div className="setup-card">
        <div className="setup-header">
          <div className="setup-logo">Humandocs</div>
          <div className="setup-tagline">Your notes, everywhere.</div>
        </div>

        <form className="setup-form" onSubmit={handleSubmit}>
          <div className="form-group">
            <label className="form-label" htmlFor="server-url">Server URL</label>
            <input
              id="server-url"
              className="form-input"
              type="text"
              placeholder="https://sync.example.com"
              value={serverUrl}
              onChange={(e) => setServerUrl(e.target.value)}
              disabled={loading}
              autoFocus
            />
          </div>

          <div className="form-group">
            <label className="form-label" htmlFor="password">Password</label>
            <input
              id="password"
              className="form-input"
              type="password"
              placeholder="Enter password"
              value={password}
              onChange={(e) => setPassword(e.target.value)}
              disabled={loading}
            />
          </div>

          {error && <div className="setup-error">{error}</div>}

          {loading ? (
            <div className="setup-spinner">
              <div className="spinner" />
              Connecting...
            </div>
          ) : (
            <button
              type="submit"
              className="btn-primary"
              disabled={!serverUrl.trim() || !password.trim()}
            >
              Connect
            </button>
          )}
        </form>
      </div>
    </div>
  );
}
