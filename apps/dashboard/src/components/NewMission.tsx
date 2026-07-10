// New-mission form (route #/new): goal (required) + an optional advanced
// section overriding backend / model / reasoning effort per role. Create POSTs
// /api/missions with the config patch (only the fields actually overridden)
// and navigates to the mission — its status is "planning", so the planning
// UI takes over the centre pane.

import { useState } from 'react';
import { api } from '../lib/api';
import { BACKEND_OPTIONS, EFFORT_OPTIONS, MODEL_PLACEHOLDERS } from '../lib/format';
import type { AgentBackend } from '../lib/types';

type RoleKey = 'orchestrator' | 'worker' | 'validatorScrutiny' | 'validatorFunctional';

const ROLE_ROWS: { key: RoleKey; label: string }[] = [
  { key: 'orchestrator', label: 'Orchestrator' },
  { key: 'worker', label: 'Worker' },
  { key: 'validatorScrutiny', label: 'Scrutiny validator' },
  { key: 'validatorFunctional', label: 'Functional validator' },
];

interface RoleOverride {
  backend: AgentBackend | '';
  model: string;
  effort: string; // '' = server default
}

const EMPTY_OVERRIDES: Record<RoleKey, RoleOverride> = {
  orchestrator: { backend: '', model: '', effort: '' },
  worker: { backend: '', model: '', effort: '' },
  validatorScrutiny: { backend: '', model: '', effort: '' },
  validatorFunctional: { backend: '', model: '', effort: '' },
};

/** Partial MissionConfig patch from the overrides; undefined when empty. */
function buildConfigPatch(
  overrides: Record<RoleKey, RoleOverride>,
  allowBelowDefaultWorkerModel: boolean,
): Record<string, unknown> | undefined {
  const patch: Record<string, unknown> = {};
  for (const { key } of ROLE_ROWS) {
    const o = overrides[key];
    const role: Record<string, string> = {};
    if (o.backend !== '') role.backend = o.backend;
    if (o.model.trim() !== '') role.model = o.model.trim();
    if (o.effort !== '') role.reasoningEffort = o.effort;
    if (Object.keys(role).length > 0) patch[key] = role;
  }
  if (allowBelowDefaultWorkerModel) patch.allowBelowDefaultWorkerModel = true;
  return Object.keys(patch).length > 0 ? patch : undefined;
}

export function NewMission() {
  const [goal, setGoal] = useState('');
  const [overrides, setOverrides] = useState(EMPTY_OVERRIDES);
  const [allowBelowDefaultWorkerModel, setAllowBelowDefaultWorkerModel] = useState(false);
  const [creating, setCreating] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const setRole = (key: RoleKey, field: keyof RoleOverride, value: string) => {
    setOverrides((prev) => ({ ...prev, [key]: { ...prev[key], [field]: value } }));
  };

  const create = async () => {
    const trimmed = goal.trim();
    if (trimmed === '' || creating) return;
    setCreating(true);
    setError(null);
    try {
      const { id } = await api.createMission(
        trimmed,
        buildConfigPatch(overrides, allowBelowDefaultWorkerModel),
      );
      window.location.hash = `#/m/${encodeURIComponent(id)}`;
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
          <span className="section-label">New mission</span>
          <button
            type="button"
            className="btn-small new-mission-back"
            onClick={() => {
              window.location.hash = '';
            }}
          >
            ← missions
          </button>
        </div>

        <div className="new-mission-body">
          <label className="new-mission-label" htmlFor="nm-goal">
            Goal
          </label>
          <textarea
            id="nm-goal"
            className="composer-input"
            rows={4}
            placeholder="What should this mission accomplish?"
            value={goal}
            autoFocus
            onChange={(e) => setGoal(e.target.value)}
          />

          <details className="new-mission-advanced">
            <summary>Advanced — backend, model &amp; effort per role</summary>
            <div className="new-mission-roles">
              {ROLE_ROWS.map(({ key, label }) => (
                <div key={key} className="role-editor-field">
                  <span className="new-mission-role">{label}</span>
                  <select
                    aria-label={`${label} backend`}
                    value={overrides[key].backend}
                    onChange={(e) => setRole(key, 'backend', e.target.value)}
                  >
                    <option value="">backend (default: claude)</option>
                    {BACKEND_OPTIONS.map((option) => (
                      <option key={option} value={option}>
                        {option}
                      </option>
                    ))}
                  </select>
                  <input
                    type="text"
                    className="mono"
                    placeholder={
                      overrides[key].backend === ''
                        ? 'model (default)'
                        : MODEL_PLACEHOLDERS[overrides[key].backend]
                    }
                    aria-label={`${label} model`}
                    value={overrides[key].model}
                    onChange={(e) => setRole(key, 'model', e.target.value)}
                  />
                  <select
                    aria-label={`${label} reasoning effort`}
                    value={overrides[key].effort}
                    onChange={(e) => setRole(key, 'effort', e.target.value)}
                  >
                    <option value="">effort (default)</option>
                    {EFFORT_OPTIONS.map((opt) => (
                      <option key={opt} value={opt}>
                        {opt}
                      </option>
                    ))}
                  </select>
                </div>
              ))}
              <label className="role-editor-optin new-mission-optin">
                <input
                  type="checkbox"
                  checked={allowBelowDefaultWorkerModel}
                  onChange={(e) => setAllowBelowDefaultWorkerModel(e.target.checked)}
                />
                allow a below-default worker model for this mission
              </label>
              <div className="role-editor-note dim">
                blank fields keep server defaults; invalid backend/model floors are refused
              </div>
            </div>
          </details>

          {error !== null && (
            <div className="picker-error" role="alert">
              Could not create mission: {error}
            </div>
          )}

          <div className="new-mission-actions">
            <button
              type="button"
              className="btn-small btn-primary"
              disabled={goal.trim() === '' || creating}
              onClick={() => void create()}
            >
              {creating ? 'Creating…' : 'Create mission'}
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}
