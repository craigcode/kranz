import type { Lens } from './lensFilter';

export type Route =
  | { view: 'projects'; repoId: null }
  | { view: 'pipeline'; repoId: string | null; lens?: Lens }
  | { view: 'new'; repoId: string | null }
  | { view: 'new-ticket'; repoId: string | null }
  | { view: 'mission'; repoId: string | null; id: string }
  | { view: 'ticket'; repoId: string | null; slug: string };

function decodeSegment(raw: string): string {
  try {
    return decodeURIComponent(raw);
  } catch {
    // A manually edited or truncated hash must not take down Mission Control.
    // Keep the literal bytes; the scoped API will return an ordinary 404.
    return raw;
  }
}

function parseRepoRoute(repoId: string, rest: string): Route {
  if (rest === '' || rest === '/') return { view: 'pipeline', repoId };
  if (rest === '/new') return { view: 'new', repoId };
  if (rest === '/new-ticket') return { view: 'new-ticket', repoId };
  if (rest === '/backlog') return { view: 'pipeline', repoId, lens: 'backlog' };
  const mission = /^\/m\/(.+)$/.exec(rest);
  if (mission) return { view: 'mission', repoId, id: decodeSegment(mission[1]) };
  const ticket = /^\/backlog\/(.+)$/.exec(rest);
  if (ticket) return { view: 'ticket', repoId, slug: decodeSegment(ticket[1]) };
  return { view: 'pipeline', repoId };
}

export function parseHash(hash = window.location.hash): Route {
  const scoped = /^#\/r\/([^/]+)(.*)$/.exec(hash);
  if (scoped) return parseRepoRoute(decodeSegment(scoped[1]), scoped[2]);

  // Historical unscoped routes remain usable against a single/default repo.
  if (hash === '#/new') return { view: 'new', repoId: null };
  if (hash === '#/new-ticket') return { view: 'new-ticket', repoId: null };
  const mission = /^#\/m\/(.+)$/.exec(hash);
  if (mission) return { view: 'mission', repoId: null, id: decodeSegment(mission[1]) };
  if (hash === '#/backlog') return { view: 'pipeline', repoId: null, lens: 'backlog' };
  const ticket = /^#\/backlog\/(.+)$/.exec(hash);
  if (ticket) return { view: 'ticket', repoId: null, slug: decodeSegment(ticket[1]) };
  if (hash !== '' && hash !== '#/' && hash !== '#') return { view: 'pipeline', repoId: null };
  return { view: 'projects', repoId: null };
}

export function repoIdFromHash(hash = window.location.hash): string | null {
  return parseHash(hash).repoId;
}

export function repoHashFor(repoId: string, suffix = ''): string {
  const tail = suffix === '' ? '' : `/${suffix.replace(/^\//, '')}`;
  return `#/r/${encodeURIComponent(repoId)}${tail}`;
}

/** Build a route inside the current repository, preserving legacy unscoped
 * routes in tests and single-repository deep links. */
export function repoHash(suffix = ''): string {
  const repoId = repoIdFromHash();
  if (repoId !== null) return repoHashFor(repoId, suffix);
  return suffix === '' ? '#/' : `#/${suffix.replace(/^\//, '')}`;
}

export const missionHash = (id: string): string => repoHash(`m/${encodeURIComponent(id)}`);
export const ticketHash = (slug: string): string =>
  repoHash(`backlog/${encodeURIComponent(slug)}`);
