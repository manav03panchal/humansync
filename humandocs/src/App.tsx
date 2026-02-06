import { useState, useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';
import SetupScreen from './components/SetupScreen';
import AppLayout from './components/AppLayout';
import './styles/app.css';

export default function App() {
  const [paired, setPaired] = useState<boolean | null>(null);

  useEffect(() => {
    invoke<boolean>('is_paired')
      .then((result) => setPaired(result))
      .catch(() => setPaired(false));
  }, []);

  // Loading state while checking pairing
  if (paired === null) {
    return (
      <div className="setup-screen">
        <div className="setup-spinner">
          <div className="spinner" />
        </div>
      </div>
    );
  }

  if (!paired) {
    return <SetupScreen onConnected={() => setPaired(true)} />;
  }

  return <AppLayout />;
}
