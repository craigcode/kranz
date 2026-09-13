import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, fireEvent, waitFor } from '@testing-library/react';
import { NewTicket } from './NewTicket';
import { ApiError } from '../lib/api';

vi.mock('../lib/api', async () => {
  const actual = await vi.importActual<typeof import('../lib/api')>('../lib/api');
  return {
    ...actual,
    api: {
      createTicket: vi.fn(),
    },
  };
});

import { api } from '../lib/api';

beforeEach(() => {
  cleanup();
  vi.mocked(api.createTicket).mockReset();
  window.location.hash = '';
});

afterEach(() => {
  window.location.hash = '';
});

function fillForm() {
  fireEvent.change(screen.getByLabelText('Slug'), { target: { value: 'fix-login-bug' } });
  fireEvent.change(screen.getByLabelText('Title'), { target: { value: 'Fix the login bug' } });
  fireEvent.change(screen.getByLabelText('Goal'), { target: { value: 'Users cannot log in' } });
  fireEvent.change(screen.getByLabelText('Context'), {
    target: { value: 'Started after last deploy' },
  });
}

describe('NewTicket', () => {
  it('issues POST /api/tickets with the entered fields on valid submission', async () => {
    vi.mocked(api.createTicket).mockResolvedValueOnce({
      slug: 'fix-login-bug',
      priority: 1,
      state: 'review',
      title: 'Fix the login bug',
      blockedBy: [],
      isBlocked: false,
      missionId: null,
    });

    render(<NewTicket />);
    fillForm();
    fireEvent.click(screen.getByRole('button', { name: 'Create ticket' }));

    await waitFor(() => {
      expect(api.createTicket).toHaveBeenCalledWith({
        slug: 'fix-login-bug',
        title: 'Fix the login bug',
        goal: 'Users cannot log in',
        context: 'Started after last deploy',
      });
    });
  });

  it('renders a duplicate-slug error on 409 without navigating', async () => {
    vi.mocked(api.createTicket).mockRejectedValueOnce(
      new ApiError(409, 'ticket slug already exists'),
    );

    render(<NewTicket />);
    fillForm();
    fireEvent.click(screen.getByRole('button', { name: 'Create ticket' }));

    expect(await screen.findByText(/ticket slug already exists/)).toBeTruthy();
    expect(window.location.hash).toBe('');
  });

  it('navigates to the created ticket on success', async () => {
    vi.mocked(api.createTicket).mockResolvedValueOnce({
      slug: 'fix-login-bug',
      priority: 1,
      state: 'review',
      title: 'Fix the login bug',
      blockedBy: [],
      isBlocked: false,
      missionId: null,
    });

    render(<NewTicket />);
    fillForm();
    fireEvent.click(screen.getByRole('button', { name: 'Create ticket' }));

    await waitFor(() => {
      expect(window.location.hash).toBe('#/backlog/fix-login-bug');
    });
  });
});
