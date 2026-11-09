import { describe, expect, it, vi } from 'vitest';
import { DEMO_STEP_COUNT, DEMO_STEP_MS, demoSnapshotAt } from './demo';
import { buildFleetModel, type BoardAskKind, type FleetModel } from './fleet';
import type { PipelineStage } from '../pipelineStage';

const BASE_MS = Date.parse('2026-10-08T12:00:00Z');

function runOneDemoLoop(): FleetModel[] {
  const models: FleetModel[] = [];
  let prev: FleetModel | null = null;
  for (let s = 0; s < DEMO_STEP_COUNT; s++) {
    const nowMs = BASE_MS + s * DEMO_STEP_MS;
    const snap = demoSnapshotAt(s, nowMs);
    const model = buildFleetModel(
      snap.tickets,
      snap.missions,
      snap.missionStates,
      nowMs,
      prev,
      '#/board/demo',
    );
    models.push(model);
    prev = model;
  }
  return models;
}

describe('demo — deterministic trajectory board demo script', () => {
  it('visits all 11 pipeline stages and all 11 ask kinds across one loop, includes a free-return failure and an abandonment, is deterministic, and makes zero fetch calls', () => {
    const fetchSpy = vi.spyOn(globalThis, 'fetch').mockImplementation(() => {
      throw new Error('fetch must not be called by demo');
    });

    try {
      const run1 = runOneDemoLoop();
      const run2 = runOneDemoLoop();

      expect(run1).toEqual(run2);
      expect(fetchSpy).not.toHaveBeenCalled();

      const seenStages = new Set<PipelineStage>();
      const seenAskKinds = new Set<BoardAskKind>();
      let sawFailureTransition = false;
      let sawAbandonTransition = false;

      for (const model of run1) {
        // Every beat has 3 non-zero counts so the demo board is immediately populated
        expect(model.counts.waiting).toBeGreaterThan(0);
        expect(model.counts.inFlight).toBeGreaterThan(0);
        expect(model.counts.landed).toBeGreaterThan(0);

        for (const c of model.crafts) {
          seenStages.add(c.stage);
        }
        for (const a of model.asks) {
          seenAskKinds.add(a.kind);
        }
        for (const tr of model.transitions) {
          if (tr.from === 'running' && tr.to === 'failed') sawFailureTransition = true;
          if (tr.to === 'abandoned') sawAbandonTransition = true;
        }
      }

      const allStages: PipelineStage[] = [
        'captured',
        'drafting',
        'needs-you',
        'wrong-plan',
        'reviewable',
        'queued',
        'running',
        'delivered',
        'landed',
        'failed',
        'abandoned',
      ];
      for (const st of allStages) {
        expect(seenStages.has(st), `expected stage ${st} to occur in demo loop`).toBe(true);
      }

      const allAskKinds: BoardAskKind[] = [
        'permission',
        'grant',
        'blocked',
        'gate',
        'revision',
        'question',
        'needs-you',
        'wrong-plan',
        'reviewable',
        'delivered',
        'failed',
      ];
      for (const kind of allAskKinds) {
        expect(seenAskKinds.has(kind), `expected ask kind ${kind} to occur in demo loop`).toBe(true);
      }

      expect(sawFailureTransition).toBe(true);
      expect(sawAbandonTransition).toBe(true);
    } finally {
      fetchSpy.mockRestore();
    }
  });
});
