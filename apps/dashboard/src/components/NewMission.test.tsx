import { beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { NewMission } from './NewMission';

vi.mock('../lib/api', () => ({
  api: {
    createMission: vi.fn(),
  },
}));

import { api } from '../lib/api';

beforeEach(() => {
  cleanup();
  vi.mocked(api.createMission).mockReset();
  window.location.hash = '';
});

describe('NewMission', () => {
  it('submits backend/model/effort and the worker lower-tier opt-in as a mission patch', async () => {
    vi.mocked(api.createMission).mockResolvedValue({ id: 'm-backend' });
    render(<NewMission />);

    fireEvent.change(screen.getByLabelText('Goal'), { target: { value: 'exercise droid lane' } });
    fireEvent.change(screen.getByLabelText('Worker backend'), { target: { value: 'droid' } });
    fireEvent.change(screen.getByLabelText('Worker model'), {
      target: { value: 'accounts/fireworks/models/glm-5p2' },
    });
    fireEvent.change(screen.getByLabelText('Worker reasoning effort'), {
      target: { value: 'high' },
    });
    fireEvent.click(
      screen.getByLabelText('allow a below-default worker model for this mission'),
    );
    fireEvent.click(screen.getByRole('button', { name: 'Create mission' }));

    await waitFor(() => {
      expect(api.createMission).toHaveBeenCalledWith('exercise droid lane', {
        worker: {
          backend: 'droid',
          model: 'accounts/fireworks/models/glm-5p2',
          reasoningEffort: 'high',
        },
        allowBelowDefaultWorkerModel: true,
      });
    });
    expect(window.location.hash).toBe('#/m/m-backend');
  });

  it('renders the server validation message inline', async () => {
    vi.mocked(api.createMission).mockRejectedValue(
      new Error('orchestrator.model "sonnet" is below the frontier-model floor'),
    );
    render(<NewMission />);

    fireEvent.change(screen.getByLabelText('Goal'), { target: { value: 'bad floor' } });
    fireEvent.click(screen.getByRole('button', { name: 'Create mission' }));

    const alert = await screen.findByRole('alert');
    expect(alert.textContent).toContain('below the frontier-model floor');
  });
});
