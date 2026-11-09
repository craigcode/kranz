import { describe, expect, it } from 'vitest';
import {
  bez,
  clock,
  extent,
  makeGeom,
  poseAt,
  timeline,
  trail,
  type BoardLayout,
  type CraftPose,
  type FlightCraft,
} from './flightPlan';

function makeCraft(overrides: Partial<FlightCraft> = {}): FlightCraft {
  return {
    createdAt: 0,
    padAt: 0,
    assembledAt: null,
    startedAt: null,
    completedAt: null,
    mergedAt: null,
    failedAt: null,
    abandonedAt: null,
    pad: 0,
    lane: 0,
    site: 0,
    afloat: false,
    ...overrides,
  };
}

const NUMERIC_POSE_KEYS: ReadonlyArray<keyof CraftPose> = [
  'x',
  'y',
  'z',
  'dx',
  'dy',
  'dz',
  'size',
  'stack',
  'burn',
  'grow',
  'roll',
  'chute',
  'anchor',
  'mid',
];

describe('flightPlan geometry and crossings', () => {
  it.each(['wide', 'tall'] as const)('preserves figure-8 orbit directions and lane order (%s)', (layout: BoardLayout) => {
    const G = makeGeom(layout);
    expect(G.layout).toBe(layout);
    expect(G.sE).toBe(1);
    expect(G.sM).toBe(-1);

    for (let lane = 0; lane < G.N; lane++) {
      if (lane > 0) {
        expect(G.rE(lane)).toBeGreaterThan(G.rE(lane - 1));
        expect(G.rM(lane)).toBeLessThan(G.rM(lane - 1));
      }

      const outbound = G.transfer(lane);
      const startPos = bez(outbound, 0);
      const endPos = bez(outbound, 1);
      expect(Math.hypot(startPos[0] - G.E.x, startPos[1] - G.E.y)).toBeCloseTo(G.rE(lane), 6);
      expect(Math.hypot(endPos[0] - G.M.x, endPos[1] - G.M.y)).toBeCloseTo(G.rM(lane), 6);

      // Verify counter-clockwise motion in Earth orbit and clockwise in lunar orbit
      const cEarth = makeCraft({ startedAt: 10_000, lane });
      const tlEarth = timeline(G, cEarth);
      const pE0 = poseAt(G, cEarth, tlEarth, 25)!;
      const pE1 = poseAt(G, cEarth, tlEarth, 25.5)!;
      const aE0 = Math.atan2(pE0.y - G.E.y, pE0.x - G.E.x);
      const aE1 = Math.atan2(pE1.y - G.E.y, pE1.x - G.E.x);
      const dE = Math.atan2(Math.sin(aE1 - aE0), Math.cos(aE1 - aE0));
      expect(dE).toBeGreaterThan(0);

      const cMoon = makeCraft({ startedAt: 10_000, completedAt: 30_000, lane });
      const tlMoon = timeline(G, cMoon);
      const tLunar = (tlMoon.arrive as number) + 5;
      const pM0 = poseAt(G, cMoon, tlMoon, tLunar)!;
      const pM1 = poseAt(G, cMoon, tlMoon, tLunar + 0.5)!;
      const aM0 = Math.atan2(pM0.y - G.M.y, pM0.x - G.M.x);
      const aM1 = Math.atan2(pM1.y - G.M.y, pM1.x - G.M.x);
      const dM = Math.atan2(Math.sin(aM1 - aM0), Math.cos(aM1 - aM0));
      expect(dM).toBeLessThan(0);
    }

    // Innermost lane at Earth (0) is outermost at Moon (0)
    expect(G.rE(0)).toBeLessThan(G.rE(G.N - 1));
    expect(G.rM(0)).toBeGreaterThan(G.rM(G.N - 1));
  });
});

describe('flightPlan 50ms life-story invariants', () => {
  const stories: Array<{ name: string; craft: FlightCraft; endSec: number }> = [
    {
      name: 'pad to landed',
      craft: makeCraft({
        createdAt: 0,
        padAt: 0,
        assembledAt: 12_000,
        startedAt: 20_000,
        completedAt: 42_000,
        mergedAt: 80_000,
        pad: 1,
        lane: 2,
        site: 3,
      }),
      endSec: 135,
    },
    {
      name: 'failed in Earth orbit through to splashdown',
      craft: makeCraft({
        createdAt: 0,
        padAt: 0,
        assembledAt: 10_000,
        startedAt: 18_000,
        failedAt: 35_000,
        pad: 0,
        lane: 1,
        afloat: false,
      }),
      endSec: 120,
    },
    {
      name: 'scrubbed on the pad',
      craft: makeCraft({
        createdAt: 0,
        padAt: 0,
        assembledAt: 8_000,
        abandonedAt: 15_000,
        pad: 2,
        lane: null,
      }),
      endSec: 25,
    },
    {
      name: 'scrubbed in orbit',
      craft: makeCraft({
        createdAt: 0,
        padAt: 0,
        assembledAt: 8_000,
        startedAt: 14_000,
        abandonedAt: 32_000,
        pad: 3,
        lane: 4,
      }),
      endSec: 45,
    },
  ];

  for (const layout of ['wide', 'tall'] as const) {
    for (const story of stories) {
      it(`keeps positions continuous and turns smooth (${layout}: ${story.name})`, () => {
        const G = makeGeom(layout);
        const tl = timeline(G, story.craft);
        let prev: CraftPose | null = null;
        let sawPose = false;

        for (let ms = 0; ms <= story.endSec * 1000; ms += 50) {
          const pose = poseAt(G, story.craft, tl, ms / 1000);
          if (!pose) {
            prev = null;
            continue;
          }
          sawPose = true;
          for (const key of NUMERIC_POSE_KEYS) {
            expect(Number.isFinite(pose[key] as number)).toBe(true);
          }
          expect(extent(pose)).toBeGreaterThanOrEqual(0);

          if (prev) {
            const step = Math.hypot(pose.x - prev.x, pose.y - prev.y, pose.z - prev.z);
            expect(step).toBeLessThanOrEqual(0.45);
            if (!pose.dead) {
              const dot = Math.max(-1, Math.min(1, pose.dx * prev.dx + pose.dy * prev.dy + pose.dz * prev.dz));
              const turn = Math.acos(dot);
              expect(turn).toBeLessThanOrEqual(0.5);
            }
          }
          prev = pose;
        }
        expect(sawPose).toBe(true);
      });
    }
  }
});

describe('flightPlan touchdown, free return, afloat, and null cases', () => {
  it('places a landing craft on its lunar surface site normal at touchdown', () => {
    const G = makeGeom('wide');
    const craft = makeCraft({
      createdAt: 0,
      padAt: 0,
      assembledAt: 5_000,
      startedAt: 10_000,
      completedAt: 25_000,
      mergedAt: 60_000,
      pad: 0,
      lane: 2,
      site: 4,
    });
    const tl = timeline(G, craft);
    expect(tl.touch).not.toBeNull();
    const touchPose = poseAt(G, craft, tl, tl.touch as number)!;
    const site = G.sites[4];
    const surface = G.surface(G.M, site.th, site.z, 0.05);

    expect(touchPose.phase).toBe('landed');
    expect(touchPose.x).toBeCloseTo(surface.pos[0], 5);
    expect(touchPose.y).toBeCloseTo(surface.pos[1], 5);
    expect(touchPose.z).toBeCloseTo(surface.pos[2], 5);
    expect(touchPose.dx).toBeCloseTo(surface.n[0], 5);
    expect(touchPose.dy).toBeCloseTo(surface.n[1], 5);
    expect(touchPose.dz).toBeCloseTo(surface.n[2], 5);
  });

  it('flies a failed craft outbound, round the Moon from thA to thO, homeward, and to splashdown', () => {
    const G = makeGeom('wide');
    const craft = makeCraft({
      createdAt: 0,
      padAt: 0,
      startedAt: 10_000,
      failedAt: 25_000,
      pad: 0,
      lane: 1,
      afloat: true,
    });
    const tl = timeline(G, craft);
    const home = tl.home as number;
    const behind = tl.behind as number;
    const back = tl.back as number;
    const reenter = tl.reenter as number;
    const splash = tl.splash as number;

    // Start and end of outbound leg
    const pOutStart = poseAt(G, craft, tl, home + 0.001)!;
    const expectedOutStart = bez(G.transfer(1), 0.001 / G.coast);
    expect(pOutStart.x).toBeCloseTo(expectedOutStart[0], 3);
    expect(pOutStart.y).toBeCloseTo(expectedOutStart[1], 3);

    // Lunar swing from thA to thO
    const pSwingStart = poseAt(G, craft, tl, behind + 0.001)!;
    const aStart = Math.atan2(pSwingStart.y - G.M.y, pSwingStart.x - G.M.x);
    expect(Math.cos(aStart - G.thA)).toBeCloseTo(1, 3);

    const pSwingEnd = poseAt(G, craft, tl, back - 0.001)!;
    const aEnd = Math.atan2(pSwingEnd.y - G.M.y, pSwingEnd.x - G.M.x);
    expect(Math.cos(aEnd - G.thO)).toBeCloseTo(1, 3);

    // Return crossing leg
    const pBackMid = poseAt(G, craft, tl, (back + reenter) / 2)!;
    const expectedBackMid = bez(G.homeLeg(1), 0.5);
    expect(pBackMid.x).toBeCloseTo(expectedBackMid[0], 4);
    expect(pBackMid.y).toBeCloseTo(expectedBackMid[1], 4);

    // Splashdown on Earth's surface
    const splashSurface = G.surface(G.E, G.thS, 0, 0.02);
    const pSplash = poseAt(G, craft, tl, splash + 0.1)!;
    expect(pSplash.phase).toBe('splash');
    expect(pSplash.x).toBeCloseTo(splashSurface.pos[0], 5);
    expect(pSplash.y).toBeCloseTo(splashSurface.pos[1], 5);
    expect(pSplash.z).toBeCloseTo(splashSurface.pos[2], 5);
  });

  it('keeps an afloat failed craft at splashdown after 10 minutes and fades non-afloat craft away', () => {
    const G = makeGeom('wide');
    const base = {
      createdAt: 0,
      padAt: 0,
      startedAt: 10_000,
      failedAt: 25_000,
      pad: 0,
      lane: 0,
    };
    const afloatCraft = makeCraft({ ...base, afloat: true });
    const sinkingCraft = makeCraft({ ...base, afloat: false });
    const tlAfloat = timeline(G, afloatCraft);
    const tlSinking = timeline(G, sinkingCraft);
    const tenMinAfterSplash = (tlAfloat.splash as number) + 600;
    const splashSurface = G.surface(G.E, G.thS, 0, 0.02);

    const stillAfloat = poseAt(G, afloatCraft, tlAfloat, tenMinAfterSplash);
    expect(stillAfloat).not.toBeNull();
    expect(stillAfloat!.phase).toBe('splash');
    expect(stillAfloat!.x).toBeCloseTo(splashSurface.pos[0], 5);
    expect(stillAfloat!.y).toBeCloseTo(splashSurface.pos[1], 5);

    expect(poseAt(G, sinkingCraft, tlSinking, tenMinAfterSplash)).toBeNull();
  });

  it('returns null before existence or without a pad, and is pure across repeated calls', () => {
    const G = makeGeom('wide');
    const uncreated = makeCraft({ createdAt: 10_000, padAt: 10_000, pad: 0 });
    const tlUncreated = timeline(G, uncreated);
    expect(poseAt(G, uncreated, tlUncreated, 5)).toBeNull();

    const padless = makeCraft({ createdAt: 0, padAt: 0, pad: null, startedAt: null });
    const tlPadless = timeline(G, padless);
    expect(poseAt(G, padless, tlPadless, 5)).toBeNull();

    const orbiting = makeCraft({ createdAt: 0, padAt: 0, startedAt: 10_000, pad: 1, lane: 3 });
    const tl1 = timeline(G, orbiting);
    const tl2 = timeline(G, orbiting);
    expect(tl1).toEqual(tl2);

    const p1 = poseAt(G, orbiting, tl1, 24.5);
    const p2 = poseAt(G, orbiting, tl2, 24.5);
    expect(p1).toEqual(p2);

    const tr = trail(G, orbiting, tl1, 24.5, 8, 0.13);
    expect(tr.length).toBe(24);
    expect(clock(65)).toBe('1:05');
    expect(clock(3661)).toBe('1:01:01');
  });
});
