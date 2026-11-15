import { beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { OrchestratorView } from './OrchestratorView';
import { useKranzStore } from '../lib/store';

vi.mock('../lib/api', async () => {
  const actual = await vi.importActual<typeof import('../lib/api')>('../lib/api');
  return {
    ...actual,
    api: {
      control: vi.fn(),
    },
  };
});

import { api } from '../lib/api';

const INITIAL_STORE_STATE = useKranzStore.getState();

beforeEach(() => {
  cleanup();
  vi.mocked(api.control).mockReset().mockResolvedValue(undefined);
  useKranzStore.setState(
    {
      ...INITIAL_STORE_STATE,
      missionId: 'm-a',
      events: [],
      state: null,
      connection: 'live',
    },
    true,
  );
});

describe('OrchestratorView Composer mission switch', () => {
  it('resets the composer text and interrupt flag on a mission switch and never sends mission A\'s draft to B (DD2)', async () => {
    const { rerender } = render(<OrchestratorView />);

    const textarea = () => screen.getByLabelText('Message to the orchestrator') as HTMLTextAreaElement;
    const interruptBox = () => screen.getByRole('checkbox') as HTMLInputElement;

    fireEvent.change(textarea(), { target: { value: 'message drafted for mission A' } });
    fireEvent.click(interruptBox());
    expect(textarea().value).toBe('message drafted for mission A');
    expect(interruptBox().checked).toBe(true);

    useKranzStore.setState({ missionId: 'm-b' });
    rerender(<OrchestratorView />);

    expect(textarea().value).toBe('');
    expect(interruptBox().checked).toBe(false);

    const sendBtn = screen.getByRole('button', { name: 'Send' }) as HTMLButtonElement;
    expect(sendBtn.disabled).toBe(true);
    fireEvent.click(sendBtn);
    expect(api.control).not.toHaveBeenCalled();
  });
});
