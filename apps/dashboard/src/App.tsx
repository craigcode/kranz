// Kranz Mission Control — four-region layout (docs/dashboard-reference.md):
// left sidebar (sessions) | top bar | status strip | centre (conversation,
// planning, or transcript) | right column (models / features / progress log).
//
// Routes (location.hash): ''  → the pipeline view (default screen, one row
// per work item across all nine stages), '#/new' → new-mission form,
// '#/m/<id>' → mission view. While the mission's status is "planning" the
// centre pane is the PlanningView (M2.5 lifecycle); once execution starts the
// live conversation view takes over. <TokenPrompt/> is global: any mutation
// hitting a 401 surfaces the paste-token bar wherever you are.

import { useEffect, useState } from 'react';
import { useKranzStore } from './lib/store';
import { resolveToken } from './lib/token';
import { PipelineView } from './components/PipelineView';
import { NewMission } from './components/NewMission';
import { NewTicket } from './components/NewTicket';
import { TopBar } from './components/TopBar';
import { StatusStrip } from './components/StatusStrip';
import { DeliveredPanel } from './components/DeliveredPanel';
import { Sidebar } from './components/Sidebar';
import { OrchestratorView } from './components/OrchestratorView';
import { PlanningView } from './components/PlanningView';
import { TranscriptView } from './components/TranscriptView';
import { ModelPanel } from './components/ModelPanel';
import { GrantRequestPanel } from './components/GrantRequestPanel';
import { RevisionPanel } from './components/RevisionPanel';
import { FeaturesPanel } from './components/FeaturesPanel';
import { ProgressLog } from './components/ProgressLog';
import { TokenPrompt } from './components/TokenPrompt';
import { TicketDetail } from './components/TicketDetail';
import type { Lens } from './lib/lensFilter';

// Capture a `#token=<t>` from `kranz serve --open` BEFORE the router reads
// the hash (resolveToken strips it and persists to sessionStorage).
resolveToken();

type Route =
  | { view: 'pipeline'; lens?: Lens }
  | { view: 'new' }
  | { view: 'new-ticket' }
  | { view: 'mission'; id: string }
  | { view: 'ticket'; slug: string };

function parseHash(): Route {
  const hash = window.location.hash;
  if (hash === '#/new') return { view: 'new' };
  if (hash === '#/new-ticket') return { view: 'new-ticket' };
  const match = /^#\/m\/(.+)$/.exec(hash);
  if (match) return { view: 'mission', id: decodeURIComponent(match[1]) };
  if (hash === '#/backlog') return { view: 'pipeline', lens: 'backlog' };
  const ticketMatch = /^#\/backlog\/(.+)$/.exec(hash);
  if (ticketMatch) return { view: 'ticket', slug: decodeURIComponent(ticketMatch[1]) };
  return { view: 'pipeline' };
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
  const selectedRun = useKranzStore((s) => s.selectedRun);
  const status = useKranzStore((s) => s.state?.mission.status ?? null);

  // Connect when the hash points at a mission. Do NOT disconnect on null
  // (pipeline / backlog / ticket routes): draftTicket already connects the
  // returned mission, and TicketDetail needs that WS feed while staying on
  // the ticket route. connectMission replaces any previous socket; explicit
  // disconnect stays available for abandon/delete callers.
  useEffect(() => {
    if (missionId === null) return;
    connectMission(missionId);
  }, [missionId, connectMission]);

  if (route.view === 'pipeline') {
    return (
      <>
        <PipelineView initialLens={route.lens} />
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
  if (route.view === 'new-ticket') {
    return (
      <>
        <NewTicket />
        <TokenPrompt />
      </>
    );
  }
  if (route.view === 'ticket') {
    return (
      <>
        <TicketDetail slug={route.slug} />
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
        {missionId !== null && status === 'complete' && (
          <DeliveredPanel missionId={missionId} status={status} />
        )}
        <div className="app-main">
          <main className="centre">{centre}</main>
          <aside className="right-col" aria-label="Mission panels">
            <ModelPanel />
            <GrantRequestPanel />
            <RevisionPanel />
            <FeaturesPanel />
            <ProgressLog />
          </aside>
        </div>
      </div>
      <TokenPrompt />
    </div>
  );
}
