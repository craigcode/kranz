// Right column: per-role backend + model + reasoning-effort from state.config.
// Clicking a row opens a small inline editor that POSTs a config-change
// control command (applies to the NEXT spawn — the engine emits config.changed).

import { useEffect, useState } from 'react';
import { useKranzStore } from '../lib/store';
import {
  BACKEND_OPTIONS,
  EFFORT_OPTIONS,
  MODEL_PLACEHOLDERS,
  effortLevel,
} from '../lib/format';
import type { AgentBackend, RoleConfig } from '../lib/types';

type ConfigRoleKey = 'orchestrator' | 'worker' | 'validatorScrutiny' | 'validatorFunctional';

interface RoleSelectionPatch {
  backend?: AgentBackend;
  model?: string;
  effort?: string;
  allowBelowDefaultWorkerModel?: boolean;
}

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
  onApply: (selection: RoleSelectionPatch) => void;
  onCancel: () => void;
}) {
  const [backend, setBackend] = useState<AgentBackend>(props.config.backend ?? 'claude');
  const [model, setModel] = useState(props.config.model);
  const [effort, setEffort] = useState(props.config.reasoningEffort);
  const [allowBelowDefaultWorkerModel, setAllowBelowDefaultWorkerModel] = useState(
    props.allowBelowDefaultWorkerModel,
  );
  const [touched, setTouched] = useState({
    backend: false,
    model: false,
    effort: false,
    allowBelowDefaultWorkerModel: false,
  });
  useEffect(() => {
    if (!touched.backend) setBackend(props.config.backend ?? 'claude');
  }, [props.config.backend, touched.backend]);
  useEffect(() => {
    if (!touched.model) setModel(props.config.model);
  }, [props.config.model, touched.model]);
  useEffect(() => {
    if (!touched.effort) setEffort(props.config.reasoningEffort);
  }, [props.config.reasoningEffort, touched.effort]);
  useEffect(() => {
    if (!touched.allowBelowDefaultWorkerModel) {
      setAllowBelowDefaultWorkerModel(props.allowBelowDefaultWorkerModel);
    }
  }, [props.allowBelowDefaultWorkerModel, touched.allowBelowDefaultWorkerModel]);
  const hasChanges = Object.values(touched).some(Boolean);
  return (
    <div className="role-editor">
      <label className="role-editor-field">
        backend
        <select
          value={backend}
          onChange={(e) => {
            setBackend(e.target.value as AgentBackend);
            setTouched((current) => ({ ...current, backend: true }));
          }}
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
          onChange={(e) => {
            setModel(e.target.value);
            setTouched((current) => ({ ...current, model: true }));
          }}
          aria-label={`${props.label} model`}
        />
      </label>
      <label className="role-editor-field">
        effort
        <select
          value={effort}
          onChange={(e) => {
            setEffort(e.target.value);
            setTouched((current) => ({ ...current, effort: true }));
          }}
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
            onChange={(e) => {
              setAllowBelowDefaultWorkerModel(e.target.checked);
              setTouched((current) => ({
                ...current,
                allowBelowDefaultWorkerModel: true,
              }));
            }}
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
          disabled={!hasChanges || (touched.model && model.trim() === '') || props.applying}
          onClick={() =>
            props.onApply({
              ...(touched.backend ? { backend } : {}),
              ...(touched.model ? { model: model.trim() } : {}),
              ...(touched.effort ? { effort } : {}),
              ...(touched.allowBelowDefaultWorkerModel
                ? { allowBelowDefaultWorkerModel }
                : {}),
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
  const missionId = useKranzStore((s) => s.missionId);
  const config = useKranzStore((s) => s.state?.config ?? null);
  const sendControl = useKranzStore((s) => s.sendControl);
  const [editing, setEditing] = useState<ConfigRoleKey | null>(null);
  const [applying, setApplying] = useState<ConfigRoleKey | null>(null);
  const [notice, setNotice] = useState<{ kind: 'ok' | 'error'; text: string } | null>(null);

  useEffect(() => {
    setEditing(null);
    setApplying(null);
    setNotice(null);
  }, [missionId]);

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
    selection: RoleSelectionPatch,
  ) => {
    setApplying(key);
    setNotice(null);
    const missionAtSubmit = missionId;
    const rolePatch: Record<string, unknown> = {};
    if (selection.backend !== undefined) rolePatch.backend = selection.backend;
    if (selection.model !== undefined) rolePatch.model = selection.model;
    if (selection.effort !== undefined) rolePatch.reasoningEffort = selection.effort;
    const patch: Record<string, unknown> = {};
    if (Object.keys(rolePatch).length > 0) patch[key] = rolePatch;
    if (key === 'worker' && selection.allowBelowDefaultWorkerModel !== undefined) {
      patch.allowBelowDefaultWorkerModel = selection.allowBelowDefaultWorkerModel;
    }
    try {
      await sendControl({ kind: 'config-change', patch });
      if (useKranzStore.getState().missionId !== missionAtSubmit) return;
      setNotice({ kind: 'ok', text: 'queued — applies to next spawn' });
      setEditing(null);
    } catch (err) {
      if (useKranzStore.getState().missionId !== missionAtSubmit) return;
      setNotice({
        kind: 'error',
        text: err instanceof Error ? err.message : String(err),
      });
    } finally {
      if (useKranzStore.getState().missionId === missionAtSubmit) setApplying(null);
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
                  key={`${missionId ?? 'none'}:${key}`}
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
