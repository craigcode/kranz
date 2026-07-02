// Right column: per-role model + reasoning-effort pips from state.config.
// Clicking a row opens a small inline editor that POSTs a config-change
// control command (applies to the NEXT spawn — the engine emits config.changed).

import { useState } from 'react';
import { useKranzStore } from '../lib/store';
import { EFFORT_OPTIONS, effortLevel } from '../lib/format';
import type { RoleConfig } from '../lib/types';

type ConfigRoleKey = 'orchestrator' | 'worker' | 'validatorScrutiny' | 'validatorFunctional';

const ROLE_ROWS: { key: ConfigRoleKey; label: string }[] = [
  { key: 'orchestrator', label: 'Orchestrator' },
  { key: 'worker', label: 'Worker' },
  { key: 'validatorScrutiny', label: 'Scrutiny' },
  { key: 'validatorFunctional', label: 'Functional' },
];

function EffortPips({ effort }: { effort: string }) {
  const level = effortLevel(effort);
  return (
    <span className="pips" title={`effort: ${effort}`}>
      {[1, 2, 3, 4, 5].map((i) => (
        <span key={i} className={`pip${i <= level ? ' pip-on' : ''}`} aria-hidden="true" />
      ))}
    </span>
  );
}

function RoleEditor(props: {
  label: string;
  config: RoleConfig;
  onApply: (model: string, effort: string) => void;
  onCancel: () => void;
}) {
  const [model, setModel] = useState(props.config.model);
  const [effort, setEffort] = useState(props.config.reasoningEffort);
  return (
    <div className="role-editor">
      <label className="role-editor-field">
        model
        <input
          type="text"
          className="mono"
          value={model}
          onChange={(e) => setModel(e.target.value)}
          aria-label={`${props.label} model`}
        />
      </label>
      <label className="role-editor-field">
        effort
        <select
          value={effort}
          onChange={(e) => setEffort(e.target.value)}
          aria-label={`${props.label} reasoning effort`}
        >
          {EFFORT_OPTIONS.map((opt) => (
            <option key={opt} value={opt}>
              {opt}
            </option>
          ))}
        </select>
      </label>
      <div className="role-editor-note dim">applies to next spawn</div>
      <div className="role-editor-actions">
        <button type="button" className="btn-small" onClick={props.onCancel}>
          Cancel
        </button>
        <button
          type="button"
          className="btn-small btn-primary"
          disabled={model.trim() === ''}
          onClick={() => props.onApply(model.trim(), effort)}
        >
          Apply
        </button>
      </div>
    </div>
  );
}

export function ModelPanel() {
  const config = useKranzStore((s) => s.state?.config ?? null);
  const sendControl = useKranzStore((s) => s.sendControl);
  const [editing, setEditing] = useState<ConfigRoleKey | null>(null);
  const [notice, setNotice] = useState<string | null>(null);

  if (!config) {
    return (
      <section className="panel">
        <div className="section-label">Models</div>
        <div className="dim panel-empty">—</div>
      </section>
    );
  }

  const apply = (key: ConfigRoleKey, model: string, effort: string) => {
    void sendControl({
      kind: 'config-change',
      patch: { [key]: { model, reasoningEffort: effort } },
    })
      .then(() => setNotice('queued — applies to next spawn'))
      .catch((err: unknown) =>
        setNotice(err instanceof Error ? err.message : String(err)),
      );
    setEditing(null);
  };

  return (
    <section className="panel">
      <div className="section-label">Models</div>
      <ul className="model-list">
        {ROLE_ROWS.map(({ key, label }) => {
          const rc = config[key];
          return (
            <li key={key}>
              <button
                type="button"
                className="model-row"
                onClick={() => setEditing(editing === key ? null : key)}
                title="click to edit (applies to next spawn)"
              >
                <span className="model-role">{label}</span>
                <span className="model-name mono">{rc.model}</span>
                <EffortPips effort={rc.reasoningEffort} />
              </button>
              {editing === key && (
                <RoleEditor
                  label={label}
                  config={rc}
                  onApply={(model, effort) => apply(key, model, effort)}
                  onCancel={() => setEditing(null)}
                />
              )}
            </li>
          );
        })}
      </ul>
      {notice !== null && (
        <div className="panel-notice dim" role="status">
          {notice}
        </div>
      )}
    </section>
  );
}
