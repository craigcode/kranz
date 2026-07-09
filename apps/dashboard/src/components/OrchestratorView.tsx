// Centre pane (default): the orchestrator conversation, rendered from the
// event stream — user.message bubbles (right), orchestrator.decision cards
// (summary + expandable detail) and orchestrator-run worker.message text.
// Auto-scroll stays pinned to the bottom unless the user scrolls up.

import { useEffect, useMemo, useRef, useState } from 'react';
import { useKranzStore } from '../lib/store';
import { renderMarkdown } from '../lib/markdown';
import { relTime } from '../lib/format';
import { buildConversation } from '../lib/conversation';

export function OrchestratorView() {
  const events = useKranzStore((s) => s.events);
  const state = useKranzStore((s) => s.state);

  const orchRuns = useMemo(() => {
    const ids = new Set<string>();
    if (state) {
      for (const run of Object.values(state.runs)) {
        if (run.role === 'orchestrator') ids.add(run.id);
      }
    }
    return ids;
  }, [state]);

  const items = useMemo(() => buildConversation(events, orchRuns), [events, orchRuns]);

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
      <div className="convo" ref={scrollRef} onScroll={onScroll}>
        {items.length === 0 && (
          <div className="convo-empty dim" role="status">
            No conversation yet. The orchestrator's decisions and replies appear here.
          </div>
        )}
        {items.map((item) => {
          if (item.kind === 'user') {
            return (
              <div key={item.seq} className="msg msg-user">
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
              <div key={item.seq} className="msg msg-orch">
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
            <div key={item.seq} className="msg msg-orch">
              <div className="orch-text">{renderMarkdown(item.text)}</div>
              <span className="msg-time dim">{relTime(item.ts)}</span>
            </div>
          );
        })}
      </div>
      <Composer />
    </div>
  );
}

function Composer() {
  const connection = useKranzStore((s) => s.connection);
  const sendControl = useKranzStore((s) => s.sendControl);
  const [text, setText] = useState('');
  const [interrupt, setInterrupt] = useState(false);
  const [sendError, setSendError] = useState<string | null>(null);
  const disabled = connection === 'lost';

  const send = async () => {
    const trimmed = text.trim();
    if (trimmed === '' || disabled) return;
    setSendError(null);
    try {
      await sendControl({ kind: 'msg', text: trimmed, interrupt });
      setText('');
      setInterrupt(false);
    } catch (err) {
      setSendError(err instanceof Error ? err.message : String(err));
    }
  };

  return (
    <div className="composer">
      <textarea
        className="composer-input"
        aria-label="Message to the orchestrator"
        placeholder={disabled ? 'Connection lost — reconnecting…' : 'Type your message…'}
        value={text}
        disabled={disabled}
        rows={3}
        onChange={(e) => setText(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === 'Enter' && !e.shiftKey) {
            e.preventDefault();
            void send();
          }
        }}
      />
      <div className="composer-row">
        <label className="composer-toggle">
          <input
            type="checkbox"
            checked={interrupt}
            disabled={disabled}
            onChange={(e) => setInterrupt(e.target.checked)}
          />
          interrupt current worker
        </label>
        {sendError !== null && <span className="composer-error">{sendError}</span>}
        {disabled && <span className="composer-hint dim">sending disabled until reconnected</span>}
        <button
          type="button"
          className="composer-send"
          disabled={disabled || text.trim() === ''}
          onClick={() => void send()}
        >
          Send
        </button>
      </div>
    </div>
  );
}
