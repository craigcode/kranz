// Flight plan geometry, craft timeline, and pose-at-time model for the
// Trajectory Board lens — docs/scoping/trajectory-board.md (slice 1).
// Pure functions only: no clocks, no DOM, no network, no module state.

export type BoardLayout = 'wide' | 'tall';

export type Point2 = [number, number];
export type Point3 = [number, number, number];
export type Bezier4 = [Point2, Point2, Point2, Point2];

export interface BodySpec {
  x: number;
  y: number;
  R: number;
}

export interface SurfaceSite {
  th: number;
  z: number;
}

export interface SurfacePoint {
  pos: Point3;
  n: Point3;
}

export interface BoardBounds {
  x0: number;
  x1: number;
  y0: number;
  y1: number;
}

export interface BoardGeom {
  layout: BoardLayout;
  E: BodySpec;
  M: BodySpec;
  N: number;
  EL: number;
  cosE: number;
  sinE: number;
  sE: 1;
  sM: -1;
  rE: (lane: number) => number;
  rM: (lane: number) => number;
  wE: (lane: number) => number;
  wM: (lane: number) => number;
  bandE: [number, number];
  bandM: [number, number];
  phi: number;
  thD: number;
  thA: number;
  thO: number;
  thR: number;
  thS: number;
  joinLead: number;
  leadDeo: number;
  pads: SurfaceSite[];
  sites: SurfaceSite[];
  project: (x: number, y: number, z?: number) => Point2;
  surface: (body: BodySpec, th: number, z?: number, h?: number) => SurfacePoint;
  ascent: (pad: number, lane: number) => { thJ: number; c: Bezier4 };
  outAt: (rd: number, ra: number) => Bezier4;
  backAt: (ra: number, rd: number) => Bezier4;
  transfer: (lane: number) => Bezier4;
  homeLeg: (lane: number) => Bezier4;
  coast: number;
  descent: (lane: number, site: number) => Bezier4;
  sweep: number;
  swing: (lane: number) => number;
  reentry: (lane: number, u: number) => Point2;
  bounds: BoardBounds;
}

/** Minimal craft timestamps (ms) and slot assignments consumed by the flight plan. */
export interface FlightCraft {
  createdAt: number | null;
  padAt: number | null;
  assembledAt: number | null;
  startedAt: number | null;
  completedAt: number | null;
  mergedAt: number | null;
  failedAt: number | null;
  abandonedAt: number | null;
  pad: number | null;
  lane: number | null;
  site: number | null;
  afloat: boolean;
}

/** Key burn and arrival times for one craft, in seconds. */
export interface CraftTimeline {
  created: number | null;
  padAt: number | null;
  assembled: number | null;
  launch: number | null;
  join: number | null;
  thJ: number | null;
  depart: number | null;
  arrive: number | null;
  deorbit: number | null;
  touch: number | null;
  fail: number | null;
  abandon: number | null;
  home: number | null;
  behind: number | null;
  back: number | null;
  reenter: number | null;
  splash: number | null;
}

export type CraftKind = 'stack' | 'csmlm' | 'lm' | 'ascent' | 'cm';

export type CraftPhase =
  | 'pad'
  | 'ascent'
  | 'earth-orbit'
  | 'coast'
  | 'lunar-orbit'
  | 'descent'
  | 'landed'
  | 'liftoff'
  | 'free-return'
  | 'splash';

export interface CraftMate {
  x: number;
  y: number;
  dx: number;
  dy: number;
  a: number;
}

export interface CraftPose {
  x: number;
  y: number;
  z: number;
  dx: number;
  dy: number;
  dz: number;
  kind: CraftKind;
  stage: 0 | 1 | 2;
  anchor: number;
  mid: number;
  size: number;
  stack: number;
  burn: number;
  grow: number;
  roll: number;
  chute: number;
  mate: CraftMate | null;
  phase: CraftPhase;
  dead: boolean;
}

const TAU = Math.PI * 2;
const DEG = Math.PI / 180;
const ASCENT = 8;
const DESCENT = 9;
const ASSEMBLE = 1.6;
const SHED = 2.4; // seconds after the lunar burn that the third stage is left behind
const STAY = 8;
const LIFT = 3.2; // the ascent stage waits for the flag, then leaves
const REENTRY = 5;
const SPLASH_HOLD = 9;
const SPLASH_FADE = 2.5;
const SCRUB_FADE = 2.4;
const SCALE = 0.5; // model units -> world units

const HEIGHT = { stack: 5.8, upper: 1.85, csmlm: 2.2, lm: 1.0, cm: 0.5 } as const;
const MID = { stack: 2.9, upper: 4.6 } as const;
const SIZE = { pad: 1, flight: 1.5, lm: 2, cm: 1.6 } as const;

export const PROFILE = {
  ASCENT,
  DESCENT,
  STAY,
  LIFT,
  REENTRY,
  SPLASH_HOLD,
  SPLASH_FADE,
  SCRUB_FADE,
  SCALE,
  HEIGHT,
  MID,
  SIZE,
} as const;

const clamp01 = (v: number): number => (v < 0 ? 0 : v > 1 ? 1 : v);
const smooth = (v: number): number => {
  const c = clamp01(v);
  return c * c * (3 - 2 * c);
};
const lerp = (a: number, b: number, u: number): number => a + (b - a) * u;
const mod = (a: number, n: number): number => ((a % n) + n) % n;

// Unit direction of travel at angle th on a circle; s = 1 counter-clockwise, -1 clockwise.
const tan = (th: number, s: number): Point2 => [-s * Math.sin(th), s * Math.cos(th)];

export function makeGeom(layout: BoardLayout = 'wide'): BoardGeom {
  const wide = layout !== 'tall';
  const E: BodySpec = wide ? { x: -10.4, y: -4.3, R: 5.4 } : { x: -3.4, y: -11.2, R: 5.4 };
  const M: BodySpec = wide ? { x: 12.6, y: 5.4, R: 3.9 } : { x: 4.4, y: 12.2, R: 3.9 };
  const N = 5;
  const EL = 18 * DEG;
  const cosE = Math.cos(EL);
  const sinE = Math.sin(EL);
  const sE = 1 as const; // the Earth is orbited counter-clockwise
  const sM = -1 as const; // and the Moon clockwise, as Apollo did, which makes the route a figure 8
  const rE = (i: number): number => E.R + 3.3 + 0.5 * i;
  // Lanes run side by side all the way round the 8, so the inside lane at the Earth is the outside lane at the Moon.
  const rM = (i: number): number => M.R + 1.6 + 0.42 * (N - 1 - i);
  const wE = (i: number): number => TAU / (22 + 1.6 * i);
  const wM = (i: number): number => TAU / (15 + 1.3 * (N - 1 - i));
  const bandE: [number, number] = [rE(0) - 0.35, rE(N - 1) + 0.35];
  const bandM: [number, number] = [rM(N - 1) - 0.35, rM(0) + 0.35];

  const phi = Math.atan2(M.y - E.y, M.x - E.x);
  const dist = Math.hypot(M.x - E.x, M.y - E.y);
  const Q = Math.PI / 2;

  // The two crossings of the 8 are the crossed tangents between the orbits.
  // Each is left a little early and joined a little late, so it bends rather than running straight.
  const beta = Math.asin(Math.min(0.95, (rE(2) + rM(2)) / dist));
  const bend = 16 * DEG;
  const thD = phi - Q + beta - bend; // leave Earth orbit for the Moon
  const thA = phi + Q + beta - bend; // lunar orbit insertion
  const thO = phi - Q - beta + bend; // leave the Moon for home (only a failed mission does)
  const thR = phi + Q - beta + bend; // back at the Earth
  const thS = Math.min(thR + 55 * DEG, 168 * DEG); // splashdown
  const joinLead = 50 * DEG;
  const leadDeo = 58 * DEG; // powered descent starts this far short of the site
  const pads: SurfaceSite[] = [58, 69, 80, 91].map((d) => ({ th: d * DEG, z: 0.35 }));

  // Landing sites: a back row and a front row, filled from the middle out.
  const back = [84, 106, 62, 128, 40, 150];
  const front = [95, 73, 117, 51, 139, 29];
  const sites: SurfaceSite[] = [];
  for (let i = 0; i < 6; i++) {
    sites.push({ th: back[i] * DEG, z: -0.45 }, { th: front[i] * DEG, z: 0.95 });
  }

  const project = (x: number, y: number, z = 0): Point2 => [x, y * cosE - z * sinE];

  // Point and outward normal on a body's surface at in-plane angle th,
  // pushed z units toward the viewer and lifted h above the ground.
  const surface = (P: BodySpec, th: number, z = 0, h = 0): SurfacePoint => {
    const s = Math.max(-0.95, Math.min(0.95, z / P.R));
    const c = Math.sqrt(1 - s * s);
    const n: Point3 = [Math.cos(th) * c, Math.sin(th) * c, s];
    const r = P.R + h;
    return { pos: [P.x + n[0] * r, P.y + n[1] * r, n[2] * r], n };
  };

  const ascent = (pad: number, lane: number): { thJ: number; c: Bezier4 } => {
    const th = pads[mod(pad, pads.length)].th;
    const r = rE(lane);
    const thJ = th + sE * joinLead;
    const p0: Point2 = [E.x + Math.cos(th) * E.R, E.y + Math.sin(th) * E.R];
    const rise = (r - E.R) * 0.78;
    const p3: Point2 = [E.x + Math.cos(thJ) * r, E.y + Math.sin(thJ) * r];
    const tj = tan(thJ, sE);
    const k = (wE(lane) * r * ASCENT) / 6; // arrive at orbital speed
    return {
      thJ,
      c: [p0, [p0[0] + Math.cos(th) * rise, p0[1] + Math.sin(th) * rise], [p3[0] - tj[0] * k, p3[1] - tj[1] * k], p3],
    };
  };

  // A crossing: from angle a0 on a circle round P0 to angle a1 on a circle round P1, leaving and arriving along the orbits.
  const leg = (
    P0: BodySpec,
    r0: number,
    a0: number,
    s0: number,
    P1: BodySpec,
    r1: number,
    a1: number,
    s1: number,
  ): Bezier4 => {
    const p0: Point2 = [P0.x + Math.cos(a0) * r0, P0.y + Math.sin(a0) * r0];
    const p3: Point2 = [P1.x + Math.cos(a1) * r1, P1.y + Math.sin(a1) * r1];
    const k = Math.hypot(p3[0] - p0[0], p3[1] - p0[1]) * 0.34;
    const t0 = tan(a0, s0);
    const t1 = tan(a1, s1);
    return [p0, [p0[0] + t0[0] * k, p0[1] + t0[1] * k], [p3[0] - t1[0] * k, p3[1] - t1[1] * k], p3];
  };

  const outAt = (rd: number, ra: number): Bezier4 => leg(E, rd, thD, sE, M, ra, thA, sM);
  const backAt = (ra: number, rd: number): Bezier4 => leg(M, ra, thO, sM, E, rd, thR, sE);
  const transfer = (lane: number): Bezier4 => outAt(rE(lane), rM(lane));
  const homeLeg = (lane: number): Bezier4 => backAt(rM(lane), rE(lane));
  const coast = Math.max(6, dist / 2.9);

  const descent = (lane: number, site: number): Bezier4 => {
    const r = rM(lane);
    const s = sites[mod(site, sites.length)];
    const a0 = s.th - sM * leadDeo;
    const p0: Point2 = [M.x + Math.cos(a0) * r, M.y + Math.sin(a0) * r];
    const t0 = tan(a0, sM);
    const k0 = (wM(lane) * r * DESCENT) / 6;
    const p3: Point2 = [M.x + Math.cos(s.th) * M.R, M.y + Math.sin(s.th) * M.R];
    return [p0, [p0[0] + t0[0] * k0, p0[1] + t0[1] * k0], [p3[0] + Math.cos(s.th) * 1.5, p3[1] + Math.sin(s.th) * 1.5], p3];
  };

  // A failed mission does not brake at the Moon: it swings round the far side and takes the other crossing back.
  const sweep = mod(sM * (thO - thA), TAU);
  const swing = (lane: number): number => sweep / (wM(lane) * 1.4);

  const reentry = (lane: number, u: number): Point2 => {
    const r = lerp(rE(lane), E.R, smooth(u));
    const th = lerp(thR, thS, 1 - (1 - u) * (1 - u));
    return [E.x + Math.cos(th) * r, E.y + Math.sin(th) * r];
  };

  const pad = 1.3;
  const ro = bandE[1];
  const mo = bandM[1];
  const bounds: BoardBounds = {
    x0: Math.min(E.x - ro, M.x - mo) - pad,
    x1: Math.max(E.x + ro, M.x + mo) + pad,
    y0: Math.min(E.y - ro, M.y - mo) * cosE - pad,
    y1: Math.max(E.y + ro, M.y + mo) * cosE + pad,
  };

  return {
    layout: wide ? 'wide' : 'tall',
    E,
    M,
    N,
    EL,
    cosE,
    sinE,
    sE,
    sM,
    rE,
    rM,
    wE,
    wM,
    bandE,
    bandM,
    phi,
    thD,
    thA,
    thO,
    thR,
    thS,
    joinLead,
    leadDeo,
    pads,
    sites,
    project,
    surface,
    ascent,
    outAt,
    backAt,
    transfer,
    homeLeg,
    coast,
    descent,
    sweep,
    swing,
    reentry,
    bounds,
  };
}

export function bez(c: Bezier4, s: number): Point2 {
  const u = 1 - s;
  const a = u * u * u;
  const b = 3 * u * u * s;
  const d = 3 * u * s * s;
  const e = s * s * s;
  return [
    a * c[0][0] + b * c[1][0] + d * c[2][0] + e * c[3][0],
    a * c[0][1] + b * c[1][1] + d * c[2][1] + e * c[3][1],
  ];
}

function bezTan(c: Bezier4, s: number): Point2 {
  const u = 1 - s;
  const x =
    3 * u * u * (c[1][0] - c[0][0]) +
    6 * u * s * (c[2][0] - c[1][0]) +
    3 * s * s * (c[3][0] - c[2][0]);
  const y =
    3 * u * u * (c[1][1] - c[0][1]) +
    6 * u * s * (c[2][1] - c[1][1]) +
    3 * s * s * (c[3][1] - c[2][1]);
  const l = Math.hypot(x, y) || 1;
  return [x / l, y / l];
}

/** Key times for one craft, in seconds. Burns wait for their orbital window. */
export function timeline(G: BoardGeom, c: FlightCraft): CraftTimeline {
  const s = (ms: number | null): number | null => (ms == null ? null : ms / 1000);
  const tl: CraftTimeline = {
    created: s(c.createdAt),
    padAt: s(c.padAt),
    assembled: s(c.assembledAt),
    launch: s(c.startedAt),
    join: null,
    thJ: null,
    depart: null,
    arrive: null,
    deorbit: null,
    touch: null,
    fail: s(c.failedAt),
    abandon: s(c.abandonedAt),
    home: null,
    behind: null,
    back: null,
    reenter: null,
    splash: null,
  };
  if (tl.launch == null || c.lane == null) return tl;
  tl.join = tl.launch + ASCENT;
  const padIndex = mod(c.pad ?? 0, G.pads.length);
  const thJ = G.pads[padIndex].th + G.sE * G.joinLead;
  tl.thJ = thJ;
  const wE = G.wE(c.lane);
  const join = tl.join;
  const burnWindow = (t0: number): number =>
    t0 + mod(G.sE * (G.thD - (thJ + G.sE * wE * (t0 - join))), TAU) / wE;

  if (tl.fail != null) {
    tl.home = burnWindow(Math.max(tl.fail + 1.0, join + 0.5));
    tl.behind = tl.home + G.coast;
    tl.back = tl.behind + G.swing(c.lane);
    tl.reenter = tl.back + G.coast;
    tl.splash = tl.reenter + REENTRY;
    return tl;
  }
  const completedSec = s(c.completedAt);
  if (completedSec == null) return tl;
  tl.depart = burnWindow(Math.max(completedSec + 1.2, join + 0.5));
  tl.arrive = tl.depart + G.coast;
  const mergedSec = s(c.mergedAt);
  if (mergedSec == null || c.site == null) return tl;
  const siteIndex = mod(c.site, G.sites.length);
  const wM = G.wM(c.lane);
  const t1 = Math.max(mergedSec + 2.0, tl.arrive + 2.0); // room to turn engine-first
  tl.deorbit =
    t1 + mod(G.sM * (G.sites[siteIndex].th - G.sM * G.leadDeo - (G.thA + G.sM * wM * (t1 - tl.arrive))), TAU) / wM;
  tl.touch = tl.deorbit + DESCENT;
  return tl;
}

/** Where the craft is at time `t` (in seconds). Returns null when there is nothing to draw. */
export function poseAt(G: BoardGeom, c: FlightCraft, tl: CraftTimeline, t: number): CraftPose | null {
  if (tl.created == null || t < tl.created) return null;
  const E = G.E;
  const M = G.M;
  const o: CraftPose = {
    x: 0,
    y: 0,
    z: 0,
    dx: 0,
    dy: 1,
    dz: 0,
    kind: 'stack',
    stage: 0,
    anchor: 0,
    mid: 0,
    size: SIZE.flight,
    stack: 1,
    burn: 0,
    grow: 1,
    roll: 0,
    chute: 0,
    mate: null,
    phase: 'pad',
    dead: false,
  };

  const lane = c.lane;
  const flying = tl.launch != null && tl.join != null && tl.thJ != null && lane != null && t >= tl.launch;

  const orbitE = (tt: number): void => {
    const ln = lane as number;
    const join = tl.join as number;
    const thJ = tl.thJ as number;
    const r = G.rE(ln);
    const th = thJ + G.sE * G.wE(ln) * (tt - join);
    const d = tan(th, G.sE);
    o.x = E.x + Math.cos(th) * r;
    o.y = E.y + Math.sin(th) * r;
    o.dx = d[0];
    o.dy = d[1];
    o.anchor = 1;
    o.stage = 2;
    o.mid = MID.upper;
    o.roll = (tt - join) * 0.35;
  };

  const along = (c4: Bezier4, s: number, flip = 0): void => {
    const p = bez(c4, s);
    const d = bezTan(c4, Math.min(Math.max(s, 0.0001), 0.9999));
    const cf = Math.cos(flip);
    const sf = Math.sin(flip);
    o.x = p[0];
    o.y = p[1];
    o.dx = d[0] * cf - d[1] * sf;
    o.dy = d[0] * sf + d[1] * cf;
    o.anchor = 1;
  };

  if (!flying) {
    if (c.pad == null || tl.padAt == null || t < tl.padAt) return null;
    const padAt = tl.padAt;
    const pad = G.pads[mod(c.pad, G.pads.length)];
    const g = G.surface(E, pad.th, pad.z, 0.05);
    o.x = g.pos[0];
    o.y = g.pos[1];
    o.z = g.pos[2];
    o.dx = g.n[0];
    o.dy = g.n[1];
    o.dz = g.n[2];
    o.grow = smooth((t - padAt) / 0.6);
    o.size = SIZE.pad;
    const drafted = (tt: number): number => 0.74 * (1 - Math.exp(-Math.max(0, tt - padAt) / 7));
    o.stack =
      tl.assembled == null || t < tl.assembled
        ? drafted(t)
        : lerp(drafted(tl.assembled), 1, smooth((t - tl.assembled) / ASSEMBLE));
    o.phase = 'pad';
    const end = tl.fail != null ? tl.fail : tl.abandon;
    if (end != null && t >= end) {
      o.dead = true;
      o.grow *= 1 - smooth((t - end - 0.3) / SCRUB_FADE);
      if (o.grow <= 0.001) return null;
    }
    return o;
  }

  const ln = lane as number;
  const launch = tl.launch as number;
  const join = tl.join as number;

  if (t < join) {
    const u = (t - launch) / ASCENT;
    const s = u * u;
    const pi = mod(c.pad ?? 0, G.pads.length);
    const a = G.ascent(pi, ln);
    along(a.c, s);
    o.z = G.pads[pi].z * (1 - smooth(u * 2.5));
    o.anchor = smooth(u * 3);
    o.mid = MID.stack * o.anchor + (MID.upper - MID.stack) * smooth((u - 0.3) / 0.45);
    o.stage = u < 0.4 ? 0 : u < 0.7 ? 1 : 2;
    o.size = lerp(SIZE.pad, SIZE.flight, smooth((u - 0.4) / 0.5));
    o.burn = smooth(u * 14) * (1 - 0.75 * smooth((u - 0.82) / 0.18));
    o.phase = 'ascent';
  } else if (
    tl.home != null &&
    tl.behind != null &&
    tl.back != null &&
    tl.reenter != null &&
    tl.splash != null &&
    t >= tl.home
  ) {
    // Free return: the same crossing out, round the far side without braking, the other crossing back.
    o.kind = 'csmlm';
    o.phase = 'free-return';
    if (t < tl.behind) {
      along(G.transfer(ln), (t - tl.home) / G.coast);
      if (t < tl.home + SHED) {
        o.kind = 'stack';
        o.stage = 2;
        o.mid = MID.upper;
      }
      o.burn = 1 - smooth((t - tl.home) / 2.2);
      o.roll = (t - tl.home) * 0.5;
    } else if (t < tl.back) {
      const th = G.thA + G.sM * G.sweep * ((t - tl.behind) / (tl.back - tl.behind));
      const d = tan(th, G.sM);
      const r = G.rM(ln);
      o.x = M.x + Math.cos(th) * r;
      o.y = M.y + Math.sin(th) * r;
      o.dx = d[0];
      o.dy = d[1];
      o.anchor = 1;
      o.roll = (t - tl.home) * 0.5;
    } else if (t < tl.reenter) {
      const u = (t - tl.back) / G.coast;
      along(G.homeLeg(ln), u);
      if (u > 0.88) {
        // Only the command module comes back, blunt end first.
        o.kind = 'cm';
        o.size = SIZE.cm;
        o.dx = -o.dx;
        o.dy = -o.dy;
      } else {
        o.roll = (t - tl.home) * 0.5;
      }
    } else if (t < tl.splash) {
      // Through the atmosphere and down under the parachutes.
      const u = (t - tl.reenter) / REENTRY;
      const p = G.reentry(ln, u);
      const q = G.reentry(ln, Math.min(1, u + 0.02));
      const th = Math.atan2(p[1] - E.y, p[0] - E.x);
      const hang = smooth((u - 0.4) / 0.45);
      let vx = q[0] - p[0];
      let vy = q[1] - p[1];
      const vl = Math.hypot(vx, vy) || 1;
      vx = lerp(-vx / vl, Math.cos(th), hang);
      vy = lerp(-vy / vl, Math.sin(th), hang);
      const l = Math.hypot(vx, vy) || 1;
      o.kind = 'cm';
      o.size = SIZE.cm;
      o.x = p[0];
      o.y = p[1];
      o.dx = vx / l;
      o.dy = vy / l;
      o.chute = smooth((u - 0.35) / 0.15);
      o.anchor = 1 - smooth((u - 0.8) / 0.2);
    } else {
      const g = G.surface(E, G.thS, 0, 0.02);
      const dt = t - tl.splash;
      o.kind = 'cm';
      o.size = SIZE.cm;
      o.x = g.pos[0];
      o.y = g.pos[1];
      o.z = g.pos[2];
      o.dx = g.n[0];
      o.dy = g.n[1];
      o.dz = g.n[2];
      o.chute = 1 - smooth(dt / 1.2);
      o.grow = c.afloat ? 1 : 1 - smooth((dt - SPLASH_HOLD) / SPLASH_FADE);
      if (o.grow <= 0.001) return null;
      o.phase = 'splash';
    }
    o.dead = true;
    return o;
  } else if (tl.depart == null || t < tl.depart) {
    orbitE(t);
    const burnAt = tl.depart != null ? tl.depart : tl.home;
    o.burn = Math.max(
      0.25 * (1 - smooth((t - join) / 0.7)),
      burnAt == null ? 0 : smooth((t - (burnAt - 1.1)) / 1.1),
    );
    o.phase = 'earth-orbit';
  } else if (tl.arrive != null && t < tl.arrive) {
    // Turn engine-first for the insertion burn at the end of the coast.
    along(
      G.transfer(ln),
      (t - tl.depart) / G.coast,
      smooth((t - (tl.arrive - 2.8)) / 1.2) * Math.PI,
    );
    if (t < tl.depart + SHED) {
      o.stage = 2;
      o.mid = MID.upper;
    } else {
      o.kind = 'csmlm';
    }
    o.burn = Math.max(1 - smooth((t - tl.depart) / 2.2), smooth((t - (tl.arrive - 1.3)) / 0.6));
    o.roll = (t - tl.depart) * 0.5;
    o.phase = 'coast';
  } else if (tl.arrive != null && (tl.deorbit == null || t < tl.deorbit)) {
    const r = G.rM(ln);
    const th = G.thA + G.sM * G.wM(ln) * (t - tl.arrive);
    const d = tan(th, G.sM);
    const flip = (1 - smooth((t - tl.arrive - 0.5) / 1.3)) * Math.PI;
    const cf = Math.cos(flip);
    const sf = Math.sin(flip);
    o.kind = 'csmlm';
    o.x = M.x + Math.cos(th) * r;
    o.y = M.y + Math.sin(th) * r;
    o.dx = d[0] * cf - d[1] * sf;
    o.dy = d[0] * sf + d[1] * cf;
    o.anchor = 1;
    o.burn = 1 - smooth((t - tl.arrive) / 0.9);
    o.roll = (t - tl.arrive) * 0.3;
    o.phase = 'lunar-orbit';
  } else if (tl.arrive != null && tl.deorbit != null && tl.touch != null && c.site != null) {
    const siteIndex = mod(c.site, G.sites.length);
    const site = G.sites[siteIndex];
    const g = G.surface(M, site.th, site.z, 0.05);
    if (t < tl.touch) {
      const u = (t - tl.deorbit) / DESCENT;
      const s = 1 - (1 - u) * (1 - u);
      const c4 = G.descent(ln, siteIndex);
      const p = bez(c4, s);
      const d = bezTan(c4, Math.min(s, 0.9999));
      const settle = smooth((u - 0.5) / 0.5);
      const k = settle * settle;
      o.kind = 'lm';
      o.size = lerp(SIZE.flight, SIZE.lm, smooth(u / 0.3));
      o.x = lerp(p[0], g.pos[0], k);
      o.y = lerp(p[1], g.pos[1], k);
      o.z = g.pos[2] * settle;
      // Engine first, swinging upright over the site.
      const flip = smooth((u - 0.04) / 0.2) * Math.PI;
      const cf = Math.cos(flip);
      const sf = Math.sin(flip);
      const up = smooth((u - 0.45) / 0.4);
      const vx = lerp(d[0] * cf - d[1] * sf, g.n[0], up);
      const vy = lerp(d[0] * sf + d[1] * cf, g.n[1], up);
      const vz = g.n[2] * up;
      const l = Math.hypot(vx, vy, vz) || 1;
      o.dx = vx / l;
      o.dy = vy / l;
      o.dz = vz / l;
      o.anchor = 1 - smooth((u - 0.6) / 0.4);
      o.burn = smooth((u - 0.2) / 0.12) * (1 - smooth((u - 0.97) / 0.03));
      const r = G.rM(ln);
      const th = G.thA + G.sM * G.wM(ln) * (t - tl.arrive);
      const dm = tan(th, G.sM);
      o.mate = {
        x: M.x + Math.cos(th) * r,
        y: M.y + Math.sin(th) * r,
        dx: dm[0],
        dy: dm[1],
        a: 1 - smooth((t - tl.deorbit - 2.5) / 5),
      };
      o.phase = 'descent';
    } else {
      const dt = t - tl.touch;
      if (dt > STAY + LIFT) return null;
      const u = clamp01((dt - STAY) / LIFT);
      const h = 5.5 * u * u;
      o.kind = 'ascent';
      o.size = SIZE.lm;
      o.x = g.pos[0] + g.n[0] * h;
      o.y = g.pos[1] + g.n[1] * h;
      o.z = g.pos[2] + g.n[2] * h;
      o.dx = g.n[0];
      o.dy = g.n[1];
      o.dz = g.n[2];
      o.burn = dt > STAY ? 1 - smooth((u - 0.6) / 0.4) : 0;
      o.grow = 1 - smooth((u - 0.55) / 0.45);
      o.phase = dt > STAY ? 'liftoff' : 'landed';
    }
  }

  const end = tl.fail != null ? tl.fail : tl.abandon;
  if (end != null && t >= end) {
    o.dead = true;
    if (tl.fail == null) {
      o.burn = 0;
      o.grow *= 1 - smooth((t - end - 0.3) / SCRUB_FADE);
      if (o.grow <= 0.001) return null;
    }
  }
  return o;
}

/** Height of what is drawn, in world units. */
export function extent(p: CraftPose): number {
  let h: number;
  if (p.kind === 'stack') {
    h = p.stage === 2 ? HEIGHT.upper : p.stage === 1 ? 3.8 : HEIGHT.stack * Math.max(0.15, p.stack);
  } else if (p.kind === 'csmlm') {
    h = HEIGHT.csmlm;
  } else if (p.kind === 'cm') {
    h = HEIGHT.cm + 1.2 * p.chute;
  } else {
    h = p.kind === 'ascent' ? 0.5 : HEIGHT.lm;
  }
  return h * SCALE * p.size * p.grow;
}

export const MOVING_PHASES: ReadonlySet<CraftPhase> = new Set<CraftPhase>([
  'ascent',
  'earth-orbit',
  'coast',
  'lunar-orbit',
  'descent',
  'free-return',
]);

/**
 * Positions every `dt` seconds from `t`, nearest first: behind the craft for
 * `dt > 0` (its trail), ahead of it for `dt < 0` (the plotted course, which
 * stops where the craft arrives).
 */
export function trail(
  G: BoardGeom,
  c: FlightCraft,
  tl: CraftTimeline,
  t: number,
  n: number,
  dt: number,
): number[] {
  const out: number[] = [];
  let first: CraftPhase | null = null;
  for (let i = 0; i < n; i++) {
    const p = poseAt(G, c, tl, t - i * dt);
    if (!p || !MOVING_PHASES.has(p.phase)) break;
    if (first == null) first = p.phase;
    if (dt < 0 && p.phase !== first) {
      out.push(p.x, p.y, p.z);
      break;
    }
    out.push(p.x, p.y, p.z);
  }
  return out;
}

export function clock(sec: number): string {
  const clamped = Math.max(0, Math.floor(sec));
  const h = Math.floor(clamped / 3600);
  const m = Math.floor((clamped % 3600) / 60);
  const s = clamped % 60;
  return (h ? `${h}:${String(m).padStart(2, '0')}` : String(m)) + `:${String(s).padStart(2, '0')}`;
}
