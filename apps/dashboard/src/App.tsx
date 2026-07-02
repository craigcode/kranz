// Kranz Mission Control — four-region layout (docs/dashboard-reference.md):
// left sidebar (sessions) | top bar | status strip | centre (conversation or
// transcript) | right column (models / features / progress log).

import { useEffect, useState } from 'react';
import { useKranzStore } from './lib/store';
import { MissionPicker } from './components/MissionPicker';
import { TopBar } from './components/TopBar';
import { StatusStrip } from './components/StatusStrip';
import { Sidebar } from './components/Sidebar';
import { OrchestratorView } from './components/OrchestratorView';
import { TranscriptView } from './components/TranscriptView';
import { ModelPanel } from './components/ModelPanel';
import { FeaturesPanel } from './components/FeaturesPanel';
import { ProgressLog } from './components/ProgressLog';

function parseHash(): string | null {
  const match = /^#\/m\/(.+)$/.exec(window.location.hash);
  return match ? decodeURIComponent(match[1]) : null;
}

function useMissionIdFromHash(): string | null {
  const [id, setId] = useState<string | null>(() => parseHash());
  useEffect(() => {
    const onHash = () => setId(parseHash());
    window.addEventListener('hashchange', onHash);
    return () => window.removeEventListener('hashchange', onHash);
  }, []);
  return id;
}

export default function App() {
  const missionId = useMissionIdFromHash();
  const connectMission = useKranzStore((s) => s.connectMission);
  const disconnect = useKranzStore((s) => s.disconnect);
  const selectedRun = useKranzStore((s) => s.selectedRun);

  useEffect(() => {
    if (missionId === null) {
      disconnect();
      return;
    }
    connectMission(missionId);
    return () => disconnect();
  }, [missionId, connectMission, disconnect]);

  if (missionId === null) return <MissionPicker />;

  return (
    <div className="app">
      <Sidebar />
      <div className="app-east">
        <TopBar />
        <StatusStrip />
        <div className="app-main">
          <main className="centre">
            {selectedRun !== null ? <TranscriptView runId={selectedRun} /> : <OrchestratorView />}
          </main>
          <aside className="right-col" aria-label="Mission panels">
            <ModelPanel />
            <FeaturesPanel />
            <ProgressLog />
          </aside>
        </div>
      </div>
    </div>
  );
}
