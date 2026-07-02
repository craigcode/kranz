// Centre pane (run selected): fetches /runs/:id/transcript and renders the
// raw Claude Code stream-json values — assistant text as text, tool_use as
// collapsed expandable rows, tool results dimmed, errors/denials red.

import { useEffect, useState } from 'react';
import { useKranzStore } from '../lib/store';
import { api } from '../lib/api';
import { fmtCost, fmtTokens, roleLabel } from '../lib/format';
import { renderMarkdown } from '../lib/markdown';
import type { TranscriptBlock, TranscriptEntry } from '../lib/types';

function blockSummary(input: unknown): string {
  let text: string;
  try {
    text = typeof input === 'string' ? input : JSON.stringify(input);
  } catch {
    text = String(input);
  }
  if (text === undefined || text === null) return '';
  const oneLine = String(text).replace(/\s+/g, ' ');
  return oneLine.length > 90 ? `${oneLine.slice(0, 89)}…` : oneLine;
}

function resultText(content: unknown): string {
  if (typeof content === 'string') return content;
  if (Array.isArray(content)) {
    return content
      .map((c) => (typeof c === 'object' && c !== null && 'text' in c ? String((c as { text: unknown }).text) : ''))
      .join('\n');
  }
  try {
    return JSON.stringify(content, null, 2);
  } catch {
    return String(content);
  }
}

function ExpandableRow(props: {
  glyph: string;
  head: string;
  body: string;
  className?: string;
}) {
  const [open, setOpen] = useState(false);
  return (
    <div className={`tr-row ${props.className ?? ''}`}>
      <button type="button" className="tr-row-head" onClick={() => setOpen(!open)}>
        <span className="tr-caret" aria-hidden="true">
          {open ? '▾' : '▸'}
        </span>
        <span className="tr-glyph">{props.glyph}</span>
        <span className="tr-head-text">{props.head}</span>
      </button>
      {open && <pre className="tr-raw">{props.body}</pre>}
    </div>
  );
}

function renderBlock(block: TranscriptBlock, key: string) {
  switch (block.type) {
    case 'text':
      return (
        <div key={key} className="tr-text">
          {renderMarkdown(block.text ?? '')}
        </div>
      );
    case 'thinking':
      return (
        <ExpandableRow
          key={key}
          className="tr-thinking"
          glyph="…"
          head={`thinking ${blockSummary(block.thinking ?? '')}`}
          body={block.thinking ?? ''}
        />
      );
    case 'tool_use': {
      let raw: string;
      try {
        raw = JSON.stringify(block.input, null, 2);
      } catch {
        raw = String(block.input);
      }
      return (
        <ExpandableRow
          key={key}
          className="tr-tool"
          glyph="⚒"
          head={`${block.name ?? 'tool'} ${blockSummary(block.input)}`}
          body={raw}
        />
      );
    }
    case 'tool_result': {
      const denied = block.is_error === true;
      return (
        <ExpandableRow
          key={key}
          className={denied ? 'tr-denied' : 'tr-result'}
          glyph={denied ? '⛔' : '↳'}
          head={denied ? `denied/error ${blockSummary(resultText(block.content))}` : blockSummary(resultText(block.content))}
          body={resultText(block.content)}
        />
      );
    }
    default:
      return null;
  }
}

function renderEntry(entry: TranscriptEntry, idx: number) {
  if (entry.type === 'assistant' || entry.type === 'user') {
    const content = entry.message?.content;
    if (typeof content === 'string') {
      return (
        <div key={idx} className="tr-text">
          {renderMarkdown(content)}
        </div>
      );
    }
    if (Array.isArray(content)) {
      return content.map((block, j) => renderBlock(block, `${idx}-${j}`));
    }
    return null;
  }
  if (entry.type === 'system' && entry.subtype === 'init') {
    return (
      <div key={idx} className="tr-meta dim mono">
        system init · model {String(entry.model ?? entry.message?.model ?? '?')}
      </div>
    );
  }
  if (entry.type === 'result') {
    return (
      <div key={idx} className="tr-meta mono">
        result · {String(entry.subtype ?? '')}
        {typeof entry.total_cost_usd === 'number' && ` · ${fmtCost(entry.total_cost_usd)}`}
        {typeof entry.num_turns === 'number' && ` · ${entry.num_turns} turns`}
      </div>
    );
  }
  return null; // skip noisy system subtypes (thinking_tokens, rate limits, …)
}

export function TranscriptView({ runId }: { runId: string }) {
  const missionId = useKranzStore((s) => s.missionId);
  const state = useKranzStore((s) => s.state);
  const selectRun = useKranzStore((s) => s.selectRun);

  const [entries, setEntries] = useState<TranscriptEntry[] | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!missionId) return;
    let cancelled = false;
    setEntries(null);
    setError(null);
    api
      .transcript(missionId, runId)
      .then((data) => {
        if (!cancelled) setEntries(data);
      })
      .catch((err: unknown) => {
        if (!cancelled) setError(err instanceof Error ? err.message : String(err));
      });
    return () => {
      cancelled = true;
    };
  }, [missionId, runId]);

  const run = state?.runs[runId];

  return (
    <div className="transcript-view">
      <div className="transcript-header">
        <button type="button" className="back-btn" onClick={() => selectRun(null)}>
          ← back
        </button>
        <span className="mono transcript-run-id">{runId}</span>
        {run && (
          <>
            <span className="transcript-meta">{roleLabel(run.role)}</span>
            <span className="transcript-meta mono">{run.model}</span>
            <span className="transcript-meta mono" title="tokens in / out">
              {fmtTokens(run.tokens.input)} in · {fmtTokens(run.tokens.output)} out
            </span>
            {run.costUsd !== undefined && (
              <span className="transcript-meta mono">{fmtCost(run.costUsd)}</span>
            )}
            {run.result !== undefined ? (
              <span className={`result-chip chip-${run.result}`}>{run.result}</span>
            ) : (
              <span className="spinner" title="running" aria-label="running" />
            )}
          </>
        )}
      </div>
      <div className="transcript-body">
        {error !== null && (
          <div className="tr-error" role="alert">
            Could not load transcript: {error}
          </div>
        )}
        {error === null && entries === null && (
          <div className="dim" role="status">
            Loading transcript…
          </div>
        )}
        {entries !== null && entries.length === 0 && (
          <div className="dim" role="status">
            Transcript is empty.
          </div>
        )}
        {entries !== null && entries.map((entry, i) => renderEntry(entry, i))}
      </div>
    </div>
  );
}
