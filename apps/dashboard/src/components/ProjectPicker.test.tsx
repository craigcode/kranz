import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { ProjectPicker } from './ProjectPicker';

vi.mock('../lib/api', () => ({
  api: { repos: vi.fn() },
}));

import { api } from '../lib/api';

const emptyActivity = {
  queued: 0,
  running: 0,
  needsInput: 0,
  completeUnmerged: 0,
  failed: 0,
};

beforeEach(() => {
  cleanup();
  window.location.hash = '';
  vi.mocked(api.repos).mockReset().mockResolvedValue([
    {
      id: 'kranz',
      root: '/work/kranz',
      displayName: 'Kranz',
      group: 'Core',
      pinned: true,
      isDefault: true,
      status: 'healthy',
      activity: { ...emptyActivity, running: 2, completeUnmerged: 1 },
    },
    {
      id: 'missing',
      root: '/work/missing',
      displayName: 'Missing repo',
      group: 'Archive',
      pinned: false,
      isDefault: false,
      status: 'unavailable',
      error: 'repository root does not exist',
      activity: emptyActivity,
    },
  ]);
});

describe('ProjectPicker', () => {
  it('groups, searches, shows activity, keeps missing repos visible, and scopes selection', async () => {
    render(<ProjectPicker />);

    const kranz = await screen.findByRole('button', { name: /Kranz/ });
    expect(screen.getByRole('heading', { name: 'Core' })).toBeTruthy();
    expect(kranz.textContent).toContain('2 running');
    expect(kranz.textContent).toContain('1 unmerged');
    expect(
      (screen.getByRole('button', { name: /Missing repo/ }) as HTMLButtonElement).disabled,
    ).toBe(true);

    fireEvent.change(screen.getByLabelText('Search projects'), {
      target: { value: 'missing' },
    });
    expect(screen.queryByRole('button', { name: /Kranz/ })).toBeNull();
    expect(screen.getByRole('button', { name: /Missing repo/ })).toBeTruthy();

    fireEvent.change(screen.getByLabelText('Search projects'), { target: { value: '' } });
    fireEvent.click(screen.getByRole('button', { name: /Kranz/ }));
    await waitFor(() => expect(window.location.hash).toBe('#/r/kranz'));
  });
});
