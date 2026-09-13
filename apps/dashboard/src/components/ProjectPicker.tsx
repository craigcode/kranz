import { useCallback, useEffect, useMemo, useState } from 'react';
import { api } from '../lib/api';
import { repoHashFor } from '../lib/routes';
import type { RepoActivity, RepoSummary } from '../lib/types';

const ACTIVITY: Array<{ key: keyof RepoActivity; label: string }> = [
  { key: 'queued', label: 'queued' },
  { key: 'running', label: 'running' },
  { key: 'needsInput', label: 'needs input' },
  { key: 'completeUnmerged', label: 'unmerged' },
  { key: 'failed', label: 'failed' },
];

export function ProjectPicker() {
  const [repos, setRepos] = useState<RepoSummary[]>([]);
  const [search, setSearch] = useState('');
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(() => {
    setError(null);
    api
      .repos()
      .then(setRepos)
      .catch((reason: unknown) => {
        setError(reason instanceof Error ? reason.message : String(reason));
      });
  }, []);

  useEffect(load, [load]);

  const groups = useMemo(() => {
    const query = search.trim().toLocaleLowerCase();
    const filtered = repos
      .filter((repo) => {
        if (query === '') return true;
        return [repo.id, repo.displayName, repo.group ?? '', repo.root]
          .join('\n')
          .toLocaleLowerCase()
          .includes(query);
      })
      .sort(
        (left, right) =>
          Number(right.pinned) - Number(left.pinned) ||
          (left.group ?? '').localeCompare(right.group ?? '') ||
          left.displayName.localeCompare(right.displayName),
      );
    const grouped = new Map<string, RepoSummary[]>();
    for (const repo of filtered) {
      const group = repo.group?.trim() || 'Projects';
      grouped.set(group, [...(grouped.get(group) ?? []), repo]);
    }
    return [...grouped.entries()];
  }, [repos, search]);

  return (
    <main className="project-picker">
      <section className="project-picker-box" aria-labelledby="project-picker-title">
        <header className="project-picker-header">
          <div>
            <span className="picker-brand mono">KRANZ</span>
            <h1 id="project-picker-title">Projects</h1>
          </div>
          <input
            className="project-search"
            type="search"
            placeholder="Search projects"
            aria-label="Search projects"
            value={search}
            onChange={(event) => setSearch(event.target.value)}
          />
        </header>
        {error !== null && (
          <div className="picker-error" role="alert">
            Could not load projects: {error}{' '}
            <button type="button" className="btn-small" onClick={load}>
              retry
            </button>
          </div>
        )}
        {error === null && repos.length === 0 && (
          <div className="picker-empty dim">No repositories are configured.</div>
        )}
        {repos.length > 0 && groups.length === 0 && (
          <div className="picker-empty dim">No projects match “{search}”.</div>
        )}
        {groups.map(([group, rows]) => (
          <section className="project-group" key={group} aria-label={group}>
            <h2>{group}</h2>
            <ul className="project-list">
              {rows.map((repo) => (
                <li key={repo.id}>
                  <button
                    type="button"
                    className={`project-row project-row--${repo.status}`}
                    disabled={repo.status !== 'healthy'}
                    onClick={() => {
                      window.location.hash = repoHashFor(repo.id);
                    }}
                  >
                    <span className="project-pin" aria-label={repo.pinned ? 'pinned' : undefined}>
                      {repo.pinned ? '★' : ''}
                    </span>
                    <span className="project-identity">
                      <span className="project-name">
                        {repo.displayName}
                        {repo.isDefault && <span className="project-default">default</span>}
                      </span>
                      <span className="project-meta mono">
                        {repo.id} · {repo.status === 'healthy' ? repo.root : repo.error}
                      </span>
                    </span>
                    <span className="project-activity">
                      {ACTIVITY.map(({ key, label }) => (
                        <span
                          key={key}
                          className={`project-count project-count--${key}`}
                          title={`${repo.activity[key]} ${label}`}
                        >
                          <strong>{repo.activity[key]}</strong> {label}
                        </span>
                      ))}
                    </span>
                  </button>
                </li>
              ))}
            </ul>
          </section>
        ))}
      </section>
    </main>
  );
}
