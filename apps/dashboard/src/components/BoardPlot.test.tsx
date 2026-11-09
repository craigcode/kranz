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

  it('does not schedule continuous rAF loops or re-bind listeners on stationary model updates, and animates only while crafts move', () => {
    vi.spyOn(HTMLCanvasElement.prototype, 'getContext').mockImplementation(
      () => makeStubContext() as unknown as RenderingContext,
    );
    const rafSpy = vi.spyOn(window, 'requestAnimationFrame').mockImplementation(() => 1);
    const addVisSpy = vi.spyOn(document, 'addEventListener');
    const removeVisSpy = vi.spyOn(document, 'removeEventListener');

    const stationaryTickets: TicketSummary[] = [
      {
        slug: 't-pad',
        priority: 1,
        state: 'review',
        title: 'Pad wait',
        blockedBy: [],
        isBlocked: false,
        missionId: 'm-pad',
      },
    ];
    const stationaryMissions: MissionSummary[] = [
      { id: 'm-pad', status: 'planning', goal: 'Pad wait', createdAt: '2026-10-08T10:00:00Z' },
    ];
    const model1 = buildFleetModel(stationaryTickets, stationaryMissions, new Map(), NOW_MS);

    const { rerender, unmount } = render(
      <BoardPlot model={model1} selectedKey={null} onSelectCraft={() => {}} />,
    );

    expect(rafSpy).not.toHaveBeenCalled();
    const visBindCount = addVisSpy.mock.calls.filter(([t]) => t === 'visibilitychange').length;
    expect(visBindCount).toBe(1);

    // Simulate 4 Hz WS/poll updates on stationary fleet + selection change:
    // zero rAF loops scheduled and visibilitychange listener is NOT torn down/re-added.
    const model2 = buildFleetModel(stationaryTickets, stationaryMissions, new Map(), NOW_MS + 250, model1);
    rerender(<BoardPlot model={model2} selectedKey="ticket-t-pad" onSelectCraft={() => {}} />);

    expect(rafSpy).not.toHaveBeenCalled();
    expect(addVisSpy.mock.calls.filter(([t]) => t === 'visibilitychange').length).toBe(1);
    expect(removeVisSpy.mock.calls.filter(([t]) => t === 'visibilitychange').length).toBe(0);

    // Add a running mission -> animation loop starts via rAF
    const movingMissions: MissionSummary[] = [
      ...stationaryMissions,
      { id: 'm-run', status: 'running', goal: 'In orbit', createdAt: '2026-10-08T10:05:00Z' },
    ];
    const movingModel = buildFleetModel(stationaryTickets, movingMissions, new Map(), NOW_MS + 500, model2);
    rerender(<BoardPlot model={movingModel} selectedKey={null} onSelectCraft={() => {}} />);
    expect(rafSpy).toHaveBeenCalledTimes(1);

    unmount();
    expect(removeVisSpy.mock.calls.filter(([t]) => t === 'visibilitychange').length).toBe(1);
  });
});
