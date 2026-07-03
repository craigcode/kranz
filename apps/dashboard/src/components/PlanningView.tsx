// Centre pane while state.mission.status === "planning" (M2.5): the planning
// conversation against the server-hosted engine. Chat entries merge the WS
// activity feed (authoritative: user.message / orchestrator.decision /
// orchestrator worker.message text) with client-local optimistic items (the
// turn just typed, the POST reply) — a local item is hidden once an identical
// event arrives, so nothing renders twice.
//
// Hosting is probed by simply calling the endpoints: a 409 "not hosted" means
// this mission is being planned from a terminal — the composer locks and a
// notice explains it. The composer mirrors the TUI's busy affordance: typing
// is safe while a turn is in flight; Enter queues and messages send in order
// when the current turn finishes.

import { useEffect, useMemo, useRef, useState } from 'react';
import { useKranzStore } from '../lib/store';
import type { PlanningLocalItem } from '../lib/store';
import { renderMarkdown } from '../lib/markdown';
import { relTime } from '../lib/format';
import { useNow } from '../lib/useNow';
import { buildConversation } from '../lib/conversation';
import type { ConvoItem } from '../lib/conversation';
import { PlanReview } from './PlanReview';

/** Normalize for local-vs-event dedupe (whitespace-insensitive). */
function norm(text: string): string {
  return text.replace(/\s+/g, ' ').trim();
}

type ChatItem =
  | { source: 'event'; sort: number; item: ConvoItem }
  | { source: 'local'; sort: number; item: PlanningLocalItem };

export function PlanningView() {
  const review = useKranzStore((s) => s.planning.review);
  if (review !== null) return <PlanReview />;
  return <PlanningChat />;
}

function PlanningChat() {
  const events = useKranzStore((s) => s.events);
  const state = useKranzStore((s) => s.state);
  const localItems = useKranzStore((s) => s.planning.localItems);
  const notHosted = useKranzStore((s) => s.planning.notHosted);

  const orchRuns = useMemo(() => {
    const ids = new Set<string>();
    if (state) {
      for (const run of Object.values(state.runs)) {
        if (run.role === 'orchestrator') ids.add(run.id);
      }
    }
    return ids;
  }, [state]);

  const items = useMemo<ChatItem[]>(() => {
    const eventItems = buildConversation(events, orchRuns);
    const userTexts = new Set<string>();
    const orchTexts = new Set<string>();
    for (const it of eventItems) {
      if (it.kind === 'user') userTexts.add(norm(it.text));
      else if (it.kind === 'orch-text') orchTexts.add(norm(it.text));
    }
    const merged: ChatItem[] = eventItems.map((item) => ({
      source: 'event',
      sort: Date.parse(item.ts),
      item,
    }));
    for (const item of localItems) {
      // Hide local copies the feed has since confirmed.
      if (item.kind === 'user' && userTexts.has(norm(item.text))) continue;
      if (item.kind === 'reply' && orchTexts.has(norm(item.text))) continue;
      merged.push({ source: 'local', sort: Date.parse(item.ts), item });
    }
    // Timestamp merge keeps local optimistic items interleaved with the feed
    // (same host, same clock); stable sort preserves arrival order on ties.
    return merged.sort((a, b) => a.sort - b.sort);
  }, [events, orchRuns, localItems]);

  const scrollRef = useRef<HTMLDivElement>(null);
  const pinnedRef = useRef(true);

  const onScroll = () => {
    const el = scrollRef.current;
    if (!el) return;
    pinnedRef.current = el.scrollHeight - el.scrollTop - el.clientHeight < 48;
  };

  useEffect(() => {
    const el = scrollRef.current;
    if (el && pinnedRef.current) el.scrollTop = el.scrollHeight;
  }, [items.length]);

  return (
    <div className="orch-view">
      {notHosted && (
        <div className="planning-not-hosted" role="status">
          <strong>Planned from a terminal.</strong> This mission's planning session is not
          hosted by this server — it is being planned from a terminal (<code>kranz plan</code>).
          You can watch the conversation here; steer it from that terminal.
        </div>
      )}
      <div className="convo" ref={scrollRef} onScroll={onScroll}>
        {items.length === 0 && (
          <div className="convo-empty dim" role="status">
            Planning conversation. Describe constraints, answer the orchestrator's questions,
            then request the plan when the scope feels right.
          </div>
        )}
        {items.map((entry) =>
          entry.source === 'event' ? (
            <EventMessage key={`e${entry.item.seq}`} item={entry.item} />
          ) : (
            <LocalMessage key={`l${entry.item.id}`} item={entry.item} />
          ),
        )}
      </div>
      <PlanningComposer disabled={notHosted} />
    </div>
  );
}

function EventMessage({ item }: { item: ConvoItem }) {
  if (item.kind === 'user') {
    return (
      <div className="msg msg-user">
        <div className="bubble bubble-user">
          {item.interrupt && <span className="interrupt-tag">interrupt</span>}
          {item.text}
        </div>
        <span className="msg-time dim">{relTime(item.ts)}</span>
      </div>
    );
  }
  if (item.kind === 'decision') {
    return (
      <div className="msg msg-orch">
        <div className="decision-card">
          <div className="decision-summary">{item.summary}</div>
          {item.detail !== undefined && item.detail !== '' && (
            <details className="decision-detail">
              <summary>detail</summary>
              {renderMarkdown(item.detail)}
            </details>
          )}
        </div>
        <span className="msg-time dim">{relTime(item.ts)}</span>
      </div>
    );
  }
  return (
    <div className="msg msg-orch">
      <div className="orch-text">{renderMarkdown(item.text)}</div>
      <span className="msg-time dim">{relTime(item.ts)}</span>
    </div>
  );
}

function LocalMessage({ item }: { item: PlanningLocalItem }) {
  if (item.kind === 'user') {
    return (
      <div className="msg msg-user">
        <div className="bubble bubble-user">{item.text}</div>
        <span className="msg-time dim">{relTime(item.ts)}</span>
      </div>
    );
  }
  if (item.kind === 'notice') {
    return (
      <div className="planning-notice" role="status">
        {item.text}
      </div>
    );
  }
  return (
    <div className="msg msg-orch">
      <div className="orch-text">{renderMarkdown(item.text)}</div>
      <span className="msg-time dim">{relTime(item.ts)}</span>
    </div>
  );
}

/** Mirrors crates/cli/src/planning_tui.rs busy_status_line wording. */
function busyStatusLine(kind: 'turn' | 'plan-request', elapsedSecs: number, queued: number): string {
  const doing = kind === 'turn' ? 'orchestrator working…' : 'requesting plan…';
  const queue =
    queued === 0
      ? ''
      : queued === 1
        ? ' — 1 message queued, sends when this turn finishes'
        : ` — ${queued} messages queued, send in order when this turn finishes`;
  return `${doing} ${elapsedSecs}s — typing is safe, Enter queues your message${queue}`;
}

function PlanningComposer({ disabled }: { disabled: boolean }) {
  const busy = useKranzStore((s) => s.planning.busy);
  const busySince = useKranzStore((s) => s.planning.busySince);
  const queued = useKranzStore((s) => s.planning.queued);
  const error = useKranzStore((s) => s.planning.error);
  const sendPlanningMessage = useKranzStore((s) => s.sendPlanningMessage);
  const requestPlan = useKranzStore((s) => s.requestPlan);
  const [text, setText] = useState('');
  const now = useNow(1000);

  const send = () => {
    const trimmed = text.trim();
    if (trimmed === '' || disabled) return;
    sendPlanningMessage(trimmed); // queues client-side while a turn is in flight
    setText('');
  };

  const elapsed = busySince !== null ? Math.max(0, Math.floor((now - busySince) / 1000)) : 0;

  return (
    <div className="composer">
      <div className="planning-status dim" role="status">
        {busy !== null ? (
          <>
            <span className="spinner" aria-hidden="true" /> {busyStatusLine(busy, elapsed, queued.length)}
          </>
        ) : (
          <>● ready — type a message, or request the plan when the scope feels right</>
        )}
      </div>
      <textarea
        className="composer-input"
        aria-label="Message to the planning orchestrator"
        placeholder={
          disabled
            ? 'Planning happens in the terminal for this mission'
            : 'Describe the mission, answer questions, add constraints…'
        }
        value={text}
        disabled={disabled}
        rows={3}
        onChange={(e) => setText(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === 'Enter' && !e.shiftKey) {
            e.preventDefault();
            send();
          }
        }}
      />
      <div className="composer-row">
        <button
          type="button"
          className="btn-small"
          disabled={disabled || busy !== null}
          title="Ask the orchestrator to emit the plan for review"
          onClick={requestPlan}
        >
          Request plan
        </button>
        {error !== null && <span className="composer-error">{error}</span>}
        <button
          type="button"
          className="composer-send"
          disabled={disabled || text.trim() === ''}
          onClick={send}
        >
          Send
        </button>
      </div>
    </div>
  );
}
