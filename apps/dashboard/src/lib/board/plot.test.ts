import { describe, expect, it } from 'vitest';
import { buildFleetModel } from './fleet';
import { makeGeom, MOVING_PHASES } from './flightPlan';
import {
  boardObstacleBoxes,
  boxOverlap,
  createBoardPlotter,
  DEFAULT_BOARD_COLORS,
  fitBoard,
  hitTestCraft,
  placeLabels,
  prepareBoardFrame,
  type LayerCanvas,
  type PlotContext2D,
} from './plot';
import type { MissionState, MissionSummary, TicketSummary } from '../types';

const NOW_MS = Date.parse('2026-10-08T12:00:00Z');

interface RecordedStroke {
  strokeStyle: string;
  lineWidth: number;
  ops: string[];
}

interface RecordingContext {
  ctx: PlotContext2D;
  strokes: RecordedStroke[];
  fillRects: Array<{ x: number; y: number; w: number; h: number; fillStyle: string }>;
  translates: Array<{ x: number; y: number }>;
}

function createRecordingContext(): RecordingContext {
  const strokes: RecordedStroke[] = [];
  const fillRects: Array<{ x: number; y: number; w: number; h: number; fillStyle: string }> = [];
  const translates: Array<{ x: number; y: number }> = [];
  let currentOps: string[] = [];

  const ctx: PlotContext2D = {
    fillStyle: '#000',
    strokeStyle: '#000',
    lineWidth: 1,
    lineCap: 'butt',
    lineJoin: 'miter',
    globalAlpha: 1,
    globalCompositeOperation: 'source-over',
    setTransform() {},
    clearRect() {},
    beginPath() {
      currentOps = [];
    },
    closePath() {
      currentOps.push('closePath');
    },
    moveTo(x, y) {
      currentOps.push(`moveTo(${x.toFixed(1)},${y.toFixed(1)})`);
    },
    lineTo(x, y) {
      currentOps.push(`lineTo(${x.toFixed(1)},${y.toFixed(1)})`);
    },
    arc(_x, _y, r) {
      if (!Number.isFinite(r) || r < 0) {
        throw new RangeError(`IndexSizeError: invalid arc radius ${r}`);
      }
      currentOps.push(`arc(${r.toFixed(1)})`);
    },
    ellipse(_x, _y, rx, ry) {
      if (!Number.isFinite(rx) || rx < 0 || !Number.isFinite(ry) || ry < 0) {
        throw new RangeError(`IndexSizeError: invalid ellipse radii ${rx}, ${ry}`);
      }
      currentOps.push('ellipse');
    },
    fill() {},
    stroke() {
      strokes.push({
        strokeStyle: typeof ctx.strokeStyle === 'string' ? ctx.strokeStyle : 'gradient',
        lineWidth: ctx.lineWidth,
        ops: [...currentOps],
      });
    },
    fillRect(x, y, w, h) {
      fillRects.push({
        x,
        y,
        w,
        h,
        fillStyle: typeof ctx.fillStyle === 'string' ? ctx.fillStyle : 'gradient',
      });
    },
    setLineDash() {},
    createRadialGradient(_x0, _y0, r0, _x1, _y1, r1) {
      if (!Number.isFinite(r0) || r0 < 0 || !Number.isFinite(r1) || r1 < 0) {
        throw new RangeError(`IndexSizeError: invalid gradient radii ${r0}, ${r1}`);
      }
      return { addColorStop() {} } as unknown as CanvasGradient;
    },
    drawImage() {},
    save() {},
    restore() {},
    translate(x, y) {
      translates.push({ x, y });
    },
    rotate() {},
  };

  return { ctx, strokes, fillRects, translates };
}

function createFakeLayer(w: number, h: number): LayerCanvas {
  const rec = createRecordingContext();
  return {
    width: w,
    height: h,
    getContext: () => rec.ctx,
  };
}

describe('plot — 2D drawing against a recording stand-in context', () => {
  it('draws hold-color rings equal to waiting drawn craft, skips null poses, draws trails only for moving phases, and draws one flag per landing site', () => {
    const tickets: TicketSummary[] = [
      // captured -> pose is null (in hangar, not drawn)
      {
        slug: 't-captured',
        priority: 2,
        state: 'new',
        title: 'Captured item',
        blockedBy: [],
        isBlocked: false,
        missionId: null,
      },
      // reviewable -> on pad, waiting (ringed), not moving
      {
        slug: 't-review',
        priority: 2,
        state: 'review',
        title: 'Reviewable item',
        blockedBy: [],
        isBlocked: false,
        missionId: 'm-review',
      },
      // drafting -> on pad, not waiting, not moving
      {
        slug: 't-draft',
        priority: 2,
        state: 'drafting',
        title: 'Drafting item',
        blockedBy: [],
        isBlocked: false,
        missionId: 'm-draft',
      },
    ];

    const missions: MissionSummary[] = [
      { id: 'm-review', status: 'planning', goal: 'Reviewable', createdAt: '2026-10-08T10:00:00Z' },
      { id: 'm-draft', status: 'planning', goal: 'Drafting', createdAt: '2026-10-08T10:05:00Z' },
      // running with grant ask -> Earth orbit (moving), waiting (ringed)
      { id: 'm-run-wait', status: 'running', goal: 'Running waiting', createdAt: '2026-10-08T10:10:00Z' },
      // running without ask -> Earth orbit (moving), not waiting
      { id: 'm-run-ok', status: 'running', goal: 'Running quiet', createdAt: '2026-10-08T10:15:00Z' },
      // delivered -> lunar orbit (moving), waiting (ringed)
      {
        id: 'm-delivered',
        status: 'complete',
        merged: false,
        goal: 'Delivered',
        createdAt: '2026-10-08T10:20:00Z',
      },
      // 2 landed missions -> 2 landing sites with flags, steady-state pose is null (after STAY+LIFT)
      {
        id: 'm-land-1',
        status: 'complete',
        merged: true,
        goal: 'Landed 1',
        createdAt: '2026-10-08T09:00:00Z',
      },
      {
        id: 'm-land-2',
        status: 'complete',
        merged: true,
        goal: 'Landed 2',
        createdAt: '2026-10-08T09:30:00Z',
      },
    ];

    const states = new Map<string, MissionState>([
      [
        'm-run-wait',
        {
          mission: {
            id: 'm-run-wait',
            status: 'running',
            goal: 'Running waiting',
            baseBranch: 'main',
            missionBranch: 'kranz/mission-m-run-wait',
            createdAt: '2026-10-08T10:10:00Z',
            validationContract: [],
            milestones: [],
          },
          runs: {},
          totals: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
          totalCostUsd: 0,
          localExecutorMilestones: 0,
          escalatedMilestones: 0,
          pendingUserMessages: [],
          recentDecisions: [],
          config: {} as MissionState['config'],
          latestPlanRevision: 1,
          lastSeq: 1,
          pendingGrantRequest: {
            milestoneId: 'ms-1',
            kind: 'command',
            command: 'cargo test',
          },
        },
      ],
    ]);

    const model = buildFleetModel(tickets, missions, states, NOW_MS);
    const { view, G } = fitBoard(960, 560, 1);
    const frame = prepareBoardFrame(model, view, G, NOW_MS, null, { still: true });

    // Null-pose crafts (t-captured, m-land-1, m-land-2) are excluded from frame.crafts
    expect(frame.crafts.some((c) => c.id === 't-captured')).toBe(false);
    expect(frame.crafts.some((c) => c.id === 'm-land-1')).toBe(false);
    expect(frame.crafts.some((c) => c.id === 'm-land-2')).toBe(false);
    expect(frame.crafts).toHaveLength(5);

    // Trails exist only on moving phases
    for (const c of frame.crafts) {
      if (MOVING_PHASES.has(c.pose.phase)) {
        expect(c.trail).not.toBeNull();
        expect(c.trail!.length).toBeGreaterThanOrEqual(6);
      } else {
        expect(c.trail).toBeNull();
      }
    }

    // 2 landed sites -> 2 flags
    expect(frame.flags).toHaveLength(2);

    const rec = createRecordingContext();
    const plotter = createBoardPlotter(rec.ctx, { width: 960, height: 560 }, createFakeLayer);
    plotter.setView(view);
    plotter.draw(frame);

    const waitingDrawnCount = frame.crafts.filter((c) => c.wait).length;
    expect(waitingDrawnCount).toBe(3); // t-review, m-run-wait, m-delivered

    const holdRingStrokes = rec.strokes.filter(
      (s) => s.strokeStyle === DEFAULT_BOARD_COLORS.hold && s.ops.some((op) => op.startsWith('arc(')),
    );
    expect(holdRingStrokes).toHaveLength(waitingDrawnCount);

    // One flag banner fillRect per landing site
    const flagRects = rec.fillRects.filter((r) => r.fillStyle === DEFAULT_BOARD_COLORS.flag);
    expect(flagRects).toHaveLength(2);

    // One triangle marker (translate) per drawn craft
    expect(rec.translates).toHaveLength(frame.crafts.length);
  });

  it('builds the route layers once for two frames at the same size/layout and rebuilds when size changes', () => {
    const model = buildFleetModel([], [], new Map(), NOW_MS);
    const rec = createRecordingContext();
    const plotter = createBoardPlotter(rec.ctx, { width: 800, height: 500 }, createFakeLayer);

    const fit1 = fitBoard(800, 500, 1);
    plotter.setView(fit1.view);
    plotter.draw(prepareBoardFrame(model, fit1.view, fit1.G, NOW_MS));
    expect(plotter.getRouteBuildCount()).toBe(1);

    // Second frame at identical size and layout reuses cached route layers
    plotter.draw(prepareBoardFrame(model, fit1.view, fit1.G, NOW_MS + 1_000));
    expect(plotter.getRouteBuildCount()).toBe(1);

    // Resize triggers route layer rebuild
    const fit2 = fitBoard(1024, 640, 1);
    plotter.setView(fit2.view);
    plotter.draw(prepareBoardFrame(model, fit2.view, fit2.G, NOW_MS + 2_000));
    expect(plotter.getRouteBuildCount()).toBe(2);
  });
});

describe('plot — pure hitTestCraft and placeLabels', () => {
  it('hitTestCraft resolves a hit, a miss, and two overlapping craft to the nearer one', () => {
    const makeItem = (key: string, px: number, py: number, z: number) => ({
      key,
      px,
      py,
      rad: 12,
      pose: { z },
    });

    const crafts = [
      makeItem('craft-a', 100, 100, 0),
      makeItem('craft-b-far', 200, 200, -0.5),
      makeItem('craft-b-near', 200, 200, 0.8),
      makeItem('craft-c', 210, 200, 0),
    ];

    // Direct hit on craft-a
    expect(hitTestCraft(104, 98, crafts)).toBe('craft-a');
    // Miss far from all crafts
    expect(hitTestCraft(500, 400, crafts)).toBeNull();
    // Exact overlap at (200, 200) resolves to craft-b-near (larger z)
    expect(hitTestCraft(200, 200, crafts)).toBe('craft-b-near');
    // Overlapping hit radii at (208, 200) resolves to craft-c (closer in 2D distance)
    expect(hitTestCraft(208, 200, crafts)).toBe('craft-c');
  });

  it.each([
    ['wide', 960, 540],
    ['tall', 480, 760],
  ] as const)('placeLabels produces non-overlapping labels that avoid obstacle boxes (%s layout)', (layout, w, h) => {
    const { view } = fitBoard(w, h, 1);
    const G = makeGeom(layout);
    const obstacles = boardObstacleBoxes(view, G, [{ x: 14, y: 12, w: 180, h: 24 }]);

    const inputs = [
      { key: 'pad-0', px: 220, py: 280, rad: 12, ground: true, nx: 0.4, ny: -0.9, w: 110, h: 28, rank: 0 },
      { key: 'pad-1', px: 232, py: 274, rad: 12, ground: true, nx: 0.2, ny: -0.98, w: 110, h: 28, rank: 0 },
      { key: 'pad-2', px: 244, py: 270, rad: 12, ground: true, nx: 0.0, ny: -1.0, w: 110, h: 28, rank: 1 },
      { key: 'orb-0', px: 310, py: 210, rad: 14, ground: false, nx: 1, ny: 0, w: 120, h: 28, rank: 0 },
      { key: 'orb-1', px: 318, py: 215, rad: 14, ground: false, nx: 0.7, ny: 0.7, w: 120, h: 22, rank: 2 },
      { key: 'orb-2', px: 305, py: 205, rad: 14, ground: false, nx: -0.7, ny: 0.7, w: 120, h: 22, rank: 2 },
    ];

    const placed = [...placeLabels(inputs, obstacles, view).values()];
    expect(placed).toHaveLength(inputs.length);

    for (let i = 0; i < placed.length; i++) {
      for (let j = i + 1; j < placed.length; j++) {
        expect(boxOverlap(placed[i].chip, placed[j].chip)).toBe(0);
      }
      for (const obs of obstacles) {
        expect(boxOverlap(placed[i].chip, obs)).toBe(0);
      }
    }
  });

  it('sanitizes tiny, zero, negative, and non-finite fitBoard dimensions and draws at 10x10 without IndexSizeError', () => {
    for (const [w, h, dpr] of [
      [10, 10, 1],
      [0, 0, 1],
      [-40, -20, -2],
      [Number.NaN, Number.POSITIVE_INFINITY, Number.NaN],
    ] as const) {
      const { view, G } = fitBoard(w, h, dpr);
      expect(Number.isFinite(view.w) && view.w > 0).toBe(true);
      expect(Number.isFinite(view.h) && view.h > 0).toBe(true);
      expect(Number.isFinite(view.dpr) && view.dpr > 0).toBe(true);
      expect(Number.isFinite(view.k) && view.k >= 0.05).toBe(true);

      const missions: MissionSummary[] = [
        { id: 'm-run', status: 'running', goal: 'Orbiting', createdAt: '2026-10-08T10:10:00Z' },
        {
          id: 'm-del',
          status: 'complete',
          merged: false,
          goal: 'Delivered',
          createdAt: '2026-10-08T10:20:00Z',
        },
        {
          id: 'm-land',
          status: 'complete',
          merged: true,
          goal: 'Landed',
          createdAt: '2026-10-08T09:00:00Z',
        },
      ];
      const model = buildFleetModel([], missions, new Map(), NOW_MS);
      const frame = prepareBoardFrame(model, view, G, NOW_MS);
      const rec = createRecordingContext();
      const plotter = createBoardPlotter(rec.ctx, { width: 10, height: 10 }, createFakeLayer);
      plotter.setView(view);
      expect(() => plotter.draw(frame)).not.toThrow();
    }
  });

  it('only assigns canvas.width and canvas.height when dimensions actually change', () => {
    let widthWrites = 0;
    let heightWrites = 0;
    let curW = 800;
    let curH = 500;
    const canvas = {
      get width() {
        return curW;
      },
      set width(v: number) {
        widthWrites += 1;
        curW = v;
      },
      get height() {
        return curH;
      },
      set height(v: number) {
        heightWrites += 1;
        curH = v;
      },
    };

    const rec = createRecordingContext();
    const plotter = createBoardPlotter(rec.ctx, canvas, createFakeLayer);
    const fitSame = fitBoard(800, 500, 1);
    plotter.setView(fitSame.view);
    expect(widthWrites).toBe(0);
    expect(heightWrites).toBe(0);

    const fitNext = fitBoard(900, 520, 2);
    plotter.setView(fitNext.view);
    expect(widthWrites).toBe(1);
    expect(heightWrites).toBe(1);

    plotter.setView(fitNext.view);
    expect(widthWrites).toBe(1);
    expect(heightWrites).toBe(1);
  });

  it('preserves prevSlots hysteresis across prepareBoardFrame calls and never overlaps labels on short viewports', () => {
    const missions: MissionSummary[] = Array.from({ length: 6 }, (_, i) => ({
      id: `m-${i}`,
      status: 'running',
      goal: `Mission ${i}`,
      createdAt: `2026-10-08T10:0${i}:00Z`,
    }));
    const model = buildFleetModel([], missions, new Map(), NOW_MS);
    const { view, G } = fitBoard(800, 150, 1);

    const frame1 = prepareBoardFrame(model, view, G, NOW_MS);
    expect(frame1.slots.size).toBe(frame1.crafts.length);

    const frame2 = prepareBoardFrame(model, view, G, NOW_MS + 50, null, {
      prevSlots: frame1.slots,
    });
    expect([...frame2.slots.entries()]).toEqual([...frame1.slots.entries()]);

    for (let i = 0; i < frame2.crafts.length; i++) {
      for (let j = i + 1; j < frame2.crafts.length; j++) {
        const a = frame2.crafts[i].chip;
        const b = frame2.crafts[j].chip;
        if (a && b) {
          expect(boxOverlap(a, b)).toBe(0);
        }
      }
    }
  });
});
