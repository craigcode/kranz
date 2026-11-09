import { describe, expect, it, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, fireEvent } from '@testing-library/react';
import { BoardPlot } from './BoardPlot';
import { buildFleetModel } from '../lib/board/fleet';
import type { PlotContext2D } from '../lib/board/plot';
import type { MissionSummary, TicketSummary } from '../lib/types';

const NOW_MS = Date.parse('2026-10-08T12:00:00Z');

function makeStubContext(): PlotContext2D {
  return {
    fillStyle: '#000',
    strokeStyle: '#000',
    lineWidth: 1,
    lineCap: 'butt',
    lineJoin: 'miter',
    globalAlpha: 1,
    globalCompositeOperation: 'source-over',
    setTransform() {},
    clearRect() {},
    beginPath() {},
    closePath() {},
    moveTo() {},
    lineTo() {},
    arc() {},
    ellipse() {},
    fill() {},
    stroke() {},
    fillRect() {},
    setLineDash() {},
    createRadialGradient() {
      return { addColorStop() {} } as unknown as CanvasGradient;
    },
    drawImage() {},
    save() {},
    restore() {},
    translate() {},
    rotate() {},
  };
}

beforeEach(() => {
  cleanup();
});

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

describe('BoardPlot', () => {
  it('degrades to nothing without throwing or logging errors when getContext("2d") returns null (jsdom)', () => {
    const errSpy = vi.spyOn(console, 'error');
    const model = buildFleetModel([], [], new Map(), NOW_MS);

    const { container } = render(
      <BoardPlot model={model} selectedKey={null} onSelectCraft={() => {}} />,
    );

    expect(container.querySelector('.trajectory-board-plot')).toBeNull();
    expect(errSpy).not.toHaveBeenCalled();
  });

  it('renders the trajectory-board-plot canvas and flight-controller labels when 2D context is available', () => {
    vi.spyOn(HTMLCanvasElement.prototype, 'getContext').mockImplementation(
      () => makeStubContext() as unknown as RenderingContext,
    );

    const tickets: TicketSummary[] = [
      {
        slug: 't-launch',
        priority: 1,
        state: 'review',
        title: 'Ready to launch',
        blockedBy: [],
        isBlocked: false,
        missionId: 'm-launch',
      },
      {
        slug: 't-land',
        priority: 1,
        state: 'done',
        title: 'Ready to land',
        blockedBy: [],
        isBlocked: false,
        missionId: 'm-land',
      },
    ];
    const missions: MissionSummary[] = [
      { id: 'm-launch', status: 'planning', goal: 'Ready to launch', createdAt: '2026-10-08T10:00:00Z' },
      {
        id: 'm-land',
        status: 'complete',
        merged: false,
        goal: 'Ready to land',
        createdAt: '2026-10-08T10:05:00Z',
      },
    ];

    const model = buildFleetModel(tickets, missions, new Map(), NOW_MS);
    const onSelect = vi.fn();

    const { container } = render(
      <BoardPlot model={model} selectedKey={null} onSelectCraft={onSelect} />,
    );

    expect(container.querySelector('canvas.trajectory-board-plot')).not.toBeNull();
    expect(screen.getByText('GO FOR LAUNCH?')).toBeTruthy();
    expect(screen.getByText('GO FOR LANDING?')).toBeTruthy();

    fireEvent.click(screen.getByText('t-launch').closest('button')!);
    expect(onSelect).toHaveBeenCalledWith('ticket-t-launch');
  });
});
