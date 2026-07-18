// New-ticket capture form (route #/new-ticket): slug, title, goal (required)
// + optional context. POSTs /api/tickets (protocol.md) via api.createTicket
// and navigates to the created ticket's row (#/backlog/<slug>) so the
// operator sees it captured. Surfaces 400 (invalid slug) and 409 (duplicate
// slug) inline with the shared picker-error styling — the same pattern
// NewMission uses for its create-mission errors.

import { useState } from 'react';
import { api } from '../lib/api';
import { repoHash, ticketHash } from '../lib/routes';

export function NewTicket() {
  const [slug, setSlug] = useState('');
  const [title, setTitle] = useState('');
  const [goal, setGoal] = useState('');
  const [context, setContext] = useState('');
  const [creating, setCreating] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const canSubmit = slug.trim() !== '' && title.trim() !== '' && !creating;

  const create = async () => {
    if (!canSubmit) return;
    setCreating(true);
    setError(null);
    try {
      const created = await api.createTicket({
        slug: slug.trim(),
        title: title.trim(),
        goal: goal.trim() !== '' ? goal.trim() : undefined,
        context: context.trim() !== '' ? context.trim() : undefined,
      });
      window.location.hash = ticketHash(created.slug);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
      setCreating(false);
    }
  };

  return (
    <div className="picker">
      <div className="picker-box new-mission">
        <div className="picker-title">
          <span className="picker-brand mono">KRANZ</span>
          <span className="section-label">New ticket</span>
          <button
            type="button"
            className="btn-small new-mission-back"
            onClick={() => {
              window.location.hash = repoHash();
            }}
          >
            ← pipeline
          </button>
        </div>

        <div className="new-mission-body">
          <label className="new-mission-label" htmlFor="nt-slug">
            Slug
          </label>
          <input
            id="nt-slug"
            type="text"
            className="mono"
            placeholder="fix-login-bug"
            value={slug}
            autoFocus
            onChange={(e) => setSlug(e.target.value)}
          />

          <label className="new-mission-label" htmlFor="nt-title">
            Title
          </label>
          <input
            id="nt-title"
            type="text"
            placeholder="Short summary of the work"
            value={title}
            onChange={(e) => setTitle(e.target.value)}
          />

          <label className="new-mission-label" htmlFor="nt-goal">
            Goal
          </label>
          <textarea
            id="nt-goal"
            className="composer-input"
            rows={3}
            placeholder="What should this ticket accomplish?"
            value={goal}
            onChange={(e) => setGoal(e.target.value)}
          />

          <label className="new-mission-label" htmlFor="nt-context">
            Context
          </label>
          <textarea
            id="nt-context"
            className="composer-input"
            rows={4}
            placeholder="Any additional context for the worker"
            value={context}
            onChange={(e) => setContext(e.target.value)}
          />

          {error !== null && (
            <div className="picker-error" role="alert">
              Could not create ticket: {error}
            </div>
          )}

          <div className="new-mission-actions">
            <button
              type="button"
              className="btn-small btn-primary"
              disabled={!canSubmit}
              onClick={() => void create()}
            >
              {creating ? 'Creating…' : 'Create ticket'}
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}
