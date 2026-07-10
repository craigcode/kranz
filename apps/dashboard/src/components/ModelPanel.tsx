// Right column: per-role backend + model + reasoning-effort from state.config.
// Clicking a row opens a small inline editor that POSTs a config-change
// control command (applies to the NEXT spawn — the engine emits config.changed).

import { useState } from 'react';
import { useKranzStore } from '../lib/store';
import {
  BACKEND_OPTIONS,
  EFFORT_OPTIONS,
  MODEL_PLACEHOLDERS,
  effortLevel,
} from '../lib/format';
import type { AgentBackend, RoleConfig } from '../lib/types';

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
  roleKey: ConfigRoleKey;
  label: string;
  config: RoleConfig;
  allowBelowDefaultWorkerModel: boolean;
  applying: boolean;
  onApply: (selection: {
    backend: AgentBackend;
    model: string;
    effort: string;
    allowBelowDefaultWorkerModel: boolean;
  }) => void;
  onCancel: () => void;
}) {
  const [backend, setBackend] = useState<AgentBackend>(props.config.backend ?? 'claude');
  const [model, setModel] = useState(props.config.model);
  const [effort, setEffort] = useState(props.config.reasoningEffort);
  const [allowBelowDefaultWorkerModel, setAllowBelowDefaultWorkerModel] = useState(
    props.allowBelowDefaultWorkerModel,
  );
  return (
    <div className="role-editor">
      <label className="role-editor-field">
        backend
        <select
          value={backend}
          onChange={(e) => setBackend(e.target.value as AgentBackend)}
          aria-label={`${props.label} backend`}
        >
          {BACKEND_OPTIONS.map((option) => (
            <option key={option} value={option}>
              {option}
            </option>
          ))}
        </select>
      </label>
      <label className="role-editor-field">
        model
        <input
          type="text"
          className="mono"
          value={model}
          placeholder={MODEL_PLACEHOLDERS[backend]}
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
      {props.roleKey === 'worker' && (
        <label className="role-editor-optin">
          <input
            type="checkbox"
            checked={allowBelowDefaultWorkerModel}
            onChange={(e) => setAllowBelowDefaultWorkerModel(e.target.checked)}
          />
          allow a below-default worker model for this mission
        </label>
      )}
      <div className="role-editor-note dim">applies to next spawn</div>
      <div className="role-editor-actions">
        <button type="button" className="btn-small" onClick={props.onCancel}>
          Cancel
        </button>
        <button
          type="button"
          className="btn-small btn-primary"
          disabled={model.trim() === '' || props.applying}
          onClick={() =>
            props.onApply({
              backend,
              model: model.trim(),
              effort,
              allowBelowDefaultWorkerModel,
            })
          }
        >
          {props.applying ? 'Applying…' : 'Apply'}
        </button>
      </div>
    </div>
  );
}

export function ModelPanel() {
  const config = useKranzStore((s) => s.state?.config ?? null);
  const sendControl = useKranzStore((s) => s.sendControl);
  const [editing, setEditing] = useState<ConfigRoleKey | null>(null);
  const [applying, setApplying] = useState<ConfigRoleKey | null>(null);
  const [notice, setNotice] = useState<{ kind: 'ok' | 'error'; text: string } | null>(null);

  if (!config) {
    return (
      <section className="panel">
        <div className="section-label">Models</div>
        <div className="dim panel-empty">—</div>
      </section>
    );
  }

  const apply = async (
    key: ConfigRoleKey,
    selection: {
      backend: AgentBackend;
      model: string;
      effort: string;
      allowBelowDefaultWorkerModel: boolean;
    },
  ) => {
    setApplying(key);
    setNotice(null);
    const patch: Record<string, unknown> = {
      [key]: {
        backend: selection.backend,
        model: selection.model,
        reasoningEffort: selection.effort,
      },
    };
    if (key === 'worker') {
      patch.allowBelowDefaultWorkerModel = selection.allowBelowDefaultWorkerModel;
    }
    try {
      await sendControl({ kind: 'config-change', patch });
      setNotice({ kind: 'ok', text: 'queued — applies to next spawn' });
      setEditing(null);
    } catch (err) {
      setNotice({
        kind: 'error',
        text: err instanceof Error ? err.message : String(err),
      });
    } finally {
      setApplying(null);
    }
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
                <span className="model-backend mono">{rc.backend ?? 'claude'}</span>
                <span className="model-name mono">{rc.model}</span>
                <EffortPips effort={rc.reasoningEffort} />
              </button>
              {editing === key && (
                <RoleEditor
                  label={label}
                  roleKey={key}
                  config={rc}
                  allowBelowDefaultWorkerModel={config.allowBelowDefaultWorkerModel}
                  applying={applying === key}
                  onApply={(selection) => void apply(key, selection)}
                  onCancel={() => setEditing(null)}
                />
              )}
            </li>
          );
        })}
      </ul>
      {notice !== null && (
        <div
          className={`panel-notice${notice.kind === 'error' ? ' panel-error' : ' dim'}`}
          role={notice.kind === 'error' ? 'alert' : 'status'}
        >
          {notice.text}
        </div>
      )}
    </section>
  );
}
