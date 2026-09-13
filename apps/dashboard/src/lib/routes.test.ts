import { describe, expect, it } from 'vitest';
import { parseHash, repoHashFor } from './routes';

describe('multi-repository dashboard routes', () => {
  it('keeps repository identity on mission, ticket, and pipeline deep links', () => {
    expect(parseHash('#/r/alpha')).toEqual({ view: 'pipeline', repoId: 'alpha' });
    expect(parseHash('#/r/alpha/m/m-same')).toEqual({
      view: 'mission',
      repoId: 'alpha',
      id: 'm-same',
    });
    expect(parseHash('#/r/beta/backlog/fix-it')).toEqual({
      view: 'ticket',
      repoId: 'beta',
      slug: 'fix-it',
    });
    expect(repoHashFor('space repo', 'new')).toBe('#/r/space%20repo/new');
  });

  it('retains historical unscoped routes for the single-repo migration path', () => {
    expect(parseHash('#/m/m-1')).toEqual({ view: 'mission', repoId: null, id: 'm-1' });
    expect(parseHash('#/backlog')).toEqual({ view: 'pipeline', repoId: null, lens: 'backlog' });
  });

  it('keeps a malformed manually edited hash from crashing the dashboard', () => {
    expect(parseHash('#/r/%/m/%E0%A4%A')).toEqual({
      view: 'mission',
      repoId: '%',
      id: '%E0%A4%A',
    });
  });
});
