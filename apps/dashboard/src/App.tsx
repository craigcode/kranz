// Kranz Mission Control — four-region layout (docs/dashboard-reference.md):
// left sidebar (sessions) | top bar | status strip | centre (conversation,
// planning, or transcript) | right column (models / features / progress log).
//
// Routes (location.hash): ''  → mission picker, '#/new' → new-mission form,
// '#/m/<id>' → mission view. While the mission's status is "planning" the
// centre pane is the PlanningView (M2.5 lifecycle); once execution starts the
// live conversation view takes over. <TokenPrompt/> is global: any mutation
// hitting a 401 surfaces the paste-token bar wherever you are.

import { useEffect, useState } from 'react';
import { useKranzStore } from './lib/store';
import { resolveToken } from './lib/token';
import { MissionPicker } from './components/MissionPicker';
import { NewMission } from './components/NewMission';
import { TopBar } from './components/TopBar';
import { StatusStrip } from './components/StatusStrip';
import { Sidebar } from './components/Sidebar';
import { OrchestratorView } from './components/OrchestratorView';
import { PlanningView } from './components/PlanningView';
import { TranscriptView } from './components/TranscriptView';
import { ModelPanel } from './components/ModelPanel';
import { FeaturesPanel } from './components/FeaturesPanel';
import { ProgressLog } from './components/ProgressLog';
import { TokenPrompt } from './components/TokenPrompt';

// Capture a `#token=<t>` from `kranz serve --open` BEFORE the router reads
// the hash (resolveToken strips it and persists to sessionStorage).
resolveToken();

type Route = { view: 'picker' } | { view: 'new' } | { view: 'mission'; id: string };

function parseHash(): Route {
  const hash = window.location.hash;
  if (hash === '#/new') return { view: 'new' };
  const match = /^#\/m\/(.+)$/.exec(hash);
  if (match) return { view: 'mission', id: decodeURIComponent(match[1]) };
  return { view: 'picker' };
}

function useRoute(): Route {
  const [route, setRoute] = useState<Route>(() => parseHash());
  useEffect(() => {
    const onHash = () => setRoute(parseHash());
    window.addEventListener('hashchange', onHash);
    return () => window.removeEventListener('hashchange', onHash);
  }, []);
  return route;
}

export default function App() {
  const route = useRoute();
  const missionId = route.view === 'mission' ? route.id : null;
  const connectMission = useKranzStore((s) => s.connectMission);
  const disconnect = useKranzStore((s) => s.disconnect);
  const selectedRun = useKranzStore((s) => s.selectedRun);
  const status = useKranzStore((s) => s.state?.mission.status ?? null);

  useEffect(() => {
    if (missionId === null) {
      disconnect();
      return;
    }
    connectMission(missionId);
    return () => disconnect();
  }, [missionId, connectMission, disconnect]);

  if (route.view === 'picker') {
    return (
      <>
        <MissionPicker />
        <TokenPrompt />
      </>
    );
  }
  if (route.view === 'new') {
    return (
      <>
        <NewMission />
        <TokenPrompt />
      </>
    );
  }

  let centre = <OrchestratorView />;
  if (selectedRun !== null) centre = <TranscriptView runId={selectedRun} />;
  else if (status === 'planning') centre = <PlanningView />;

  return (
    <div className="app">
      <Sidebar />
      <div className="app-east">
        <TopBar />
        <StatusStrip />
        <div className="app-main">
          <main className="centre">{centre}</main>
          <aside className="right-col" aria-label="Mission panels">
            <ModelPanel />
            <FeaturesPanel />
            <ProgressLog />
          </aside>
        </div>
      </div>
      <TokenPrompt />
    </div>
  );
}
