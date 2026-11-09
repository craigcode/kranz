// 2D canvas plot, framing, hit-testing, and non-overlapping chip placement for the
// Trajectory Board lens — docs/scoping/trajectory-board.md (slice 4).
// Pure functions for framing, frame preparation, hit-testing, and label placement,
// plus a 2D canvas renderer ported from trajectory-board-prototype/src/chart.js & ui.js.

import type { FleetCraft, FleetModel } from './fleet';
import {
  bez,
  extent,
  makeGeom,
  MOVING_PHASES,
  poseAt,
  PROFILE,
  timeline,
  trail,
  type BoardGeom,
  type BoardLayout,
  type BodySpec,
  type CraftPose,
  type CraftTimeline,
  type Point2,
} from './flightPlan';

const TAU = Math.PI * 2;
const clamp01 = (v: number): number => (v < 0 ? 0 : v > 1 ? 1 : v);
const mod = (a: number, n: number): number => ((a % n) + n) % n;

export interface BoardView {
  w: number;
  h: number;
  dpr: number;
  k: number;
  cx: number;
  cy: number;
  layout: BoardLayout;
}

export interface BoardColors {
  ink: string;
  dim: string;
  line: string;
  line2: string;
  hold: string;
  trackAlpha: number;
  outlineAlpha: number;
  burn: string;
  earthGlow: string;
  moonGlow: string;
  flag: string;
}

export const DEFAULT_BOARD_COLORS: BoardColors = {
  ink: '#eef4fa',
  dim: 'rgba(238, 244, 250, 0.7)',
  line: 'rgba(238, 244, 250, 0.2)',
  line2: 'rgba(238, 244, 250, 0.52)',
  hold: '#ff7362',
  trackAlpha: 0.075,
  outlineAlpha: 0.3,
  burn: '#ffb75a',
  earthGlow: 'rgba(120, 190, 255, 0.3)',
  moonGlow: 'rgba(226, 232, 240, 0.16)',
  flag: '#e0633a',
};

export interface ChipBox {
  x: number;
  y: number;
  w: number;
  h: number;
}

export interface DrawnCraft {
  key: string;
  id: string;
  label: string;
  chipPhrase: string;
  fleetCraft: FleetCraft;
  tl: CraftTimeline;
  pose: CraftPose;
  wait: boolean;
  px: number;
  py: number;
  rad: number;
  ground: boolean;
  nx: number;
  ny: number;
  selected: boolean;
  trail: number[] | null;
  ahead: number[] | null;
  chip: ChipBox | null;
}

export interface BoardFrame {
  G: BoardGeom;
  view: BoardView;
  crafts: DrawnCraft[];
  flags: Point2[];
  slots: Map<string, number>;
  wall: number;
  still: boolean;
}

export interface LabelCandidateInput {
  key: string;
  px: number;
  py: number;
  rad: number;
  ground: boolean;
  nx: number;
  ny: number;
  w: number;
  h: number;
  rank: number;
  prevSlot?: number;
}

export interface PlacedLabel {
  key: string;
  chip: ChipBox;
  slot: number;
}

/** Computes the board layout ('wide' or 'tall'), geometry, and pixel fit for a viewport. */
export function fitBoard(w: number, h: number, dpr = 1): { view: BoardView; G: BoardGeom } {
  const safeW = Number.isFinite(w) && w > 0 ? w : 800;
  const safeH = Number.isFinite(h) && h > 0 ? h : 500;
  const layout: BoardLayout = safeW / safeH < 0.95 ? 'tall' : 'wide';
  const G = makeGeom(layout);
  const b = G.bounds;
  const k = Math.max(0.05, Math.min((safeW - 20) / (b.x1 - b.x0), (safeH - 20) / (b.y1 - b.y0)));
  const safeDpr = Number.isFinite(dpr) && dpr > 0 ? dpr : 1;
  const clampedDpr = Math.min(Math.max(1, safeDpr), 2);
  const view: BoardView = {
    w: safeW,
    h: safeH,
    dpr: clampedDpr,
    k,
    cx: (b.x0 + b.x1) / 2,
    cy: (b.y0 + b.y1) / 2,
    layout,
  };
  return { view, G };
}

/** Projects a 3D world point onto CSS pixels in `view`. */
export function toPx(view: BoardView, G: BoardGeom, x: number, y: number, z = 0): Point2 {
  const p = G.project(x, y, z);
  return [view.w / 2 + (p[0] - view.cx) * view.k, view.h / 2 - (p[1] - view.cy) * view.k];
}

/** Area of intersection between two axis-aligned boxes, or 0 when disjoint. */
export function boxOverlap(a: ChipBox, b: ChipBox): number {
  const w = Math.min(a.x + a.w, b.x + b.w) - Math.max(a.x, b.x);
  const h = Math.min(a.y + a.h, b.y + b.h) - Math.max(a.y, b.y);
  return w > 0 && h > 0 ? w * h : 0;
}

/** Obstacle boxes protecting the launch pads on Earth and landing sites on the Moon. */
export function boardObstacleBoxes(
  view: BoardView,
  G: BoardGeom,
  extra: ChipBox[] = [],
): ChipBox[] {
  const obstacles: ChipBox[] = [...extra];
  const specs: Array<[BodySpec, number, number, number]> = [
    [G.E, 52, 116, 3.0],
    [G.M, 24, 156, 1.3],
  ];
  const rad = Math.PI / 180;
  for (const [P, a0, a1, top] of specs) {
    const lo = P.R * Math.min(Math.sin(a0 * rad), Math.sin(a1 * rad));
    const a = toPx(view, G, P.x + Math.cos(a1 * rad) * (P.R + top), P.y + lo, 0);
    const b = toPx(view, G, P.x + Math.cos(a0 * rad) * (P.R + top), P.y + P.R + top, 0);
    const x = Math.min(a[0], b[0]);
    const y = Math.min(a[1], b[1]);
    const w = Math.abs(b[0] - a[0]);
    const h = Math.abs(a[1] - b[1]);
    obstacles.push({ x, y, w, h });
  }
  return obstacles;
}

function labelCandidates(it: LabelCandidateInput): ChipBox[] {
  const { w, h } = it;
  const out: ChipBox[] = [];
  if (it.ground) {
    const len = Math.hypot(it.nx, it.ny) || 1;
    const nx = it.nx / len;
    const ny = it.ny / len;
    const tipX = it.px + nx * (it.rad + 6);
    const tipY = it.py + ny * (it.rad + 6);
    for (let lvl = 0; lvl < 6; lvl++) {
      const d = lvl * (h + 3) + h / 2 + 2;
      const x = tipX + nx * d - w / 2;
      const y = tipY + ny * d - h / 2;
      out.push({ x, y, w, h });
      if (lvl < 3) {
        out.push({ x: x + w / 2 + 8, y, w, h }, { x: x - w / 2 - 8, y, w, h });
      }
    }
  } else {
    const d = it.rad + 9;
    const e = d * 0.72;
    out.push(
      { x: it.px + d, y: it.py - h / 2, w, h },
      { x: it.px - d - w, y: it.py - h / 2, w, h },
      { x: it.px + e, y: it.py - e - h, w, h },
      { x: it.px - e - w, y: it.py - e - h, w, h },
      { x: it.px + e, y: it.py + e, w, h },
      { x: it.px - e - w, y: it.py + e, w, h },
      { x: it.px - w / 2, y: it.py - d - h, w, h },
      { x: it.px - w / 2, y: it.py + d, w, h },
    );
  }
  // Additional rings so crowded fleets always find a zero-overlap slot
  for (let ring = 1; ring <= 8; ring++) {
    const dist = it.rad + 14 + ring * (h + 8);
    const steps = 8 + ring * 4;
    for (let s = 0; s < steps; s++) {
      const ang = (s / steps) * TAU;
      const cx = it.px + Math.cos(ang) * (dist + w * 0.35);
      const cy = it.py + Math.sin(ang) * dist;
      out.push({ x: cx - w / 2, y: cy - h / 2, w, h });
    }
  }
  return out;
}

/**
 * Pure non-overlapping label placement. Places higher-priority (waiting, then selected)
 * labels first and guarantees no two placed labels overlap each other or an obstacle box.
 */
export function placeLabels(
  list: LabelCandidateInput[],
  obstacles: ChipBox[],
  view: Pick<BoardView, 'w' | 'h'>,
): Map<string, PlacedLabel> {
  const placed: ChipBox[] = obstacles.slice();
  const order = list.slice().sort((a, b) => a.rank - b.rank || a.px - b.px);
  const result = new Map<string, PlacedLabel>();

  for (const it of order) {
    const cand = labelCandidates(it);
    const score = (r: ChipBox): number => {
      let s = 0;
      for (const p of placed) {
        const ov = boxOverlap(r, p);
        if (ov > 0) s += 1_000_000 + ov * 100;
      }
      for (const o of list) {
        if (o.key !== it.key) {
          s +=
            boxOverlap(r, {
              x: o.px - o.rad,
              y: o.py - o.rad,
              w: o.rad * 2,
              h: o.rad * 2,
            }) * 0.7;
        }
      }
      if (r.x < 4) s += (4 - r.x) * 60;
      if (r.y < 4) s += (4 - r.y) * 60;
      if (r.x + r.w > view.w - 4) s += (r.x + r.w - view.w + 4) * 60;
      if (r.y + r.h > view.h - 4) s += (r.y + r.h - view.h + 4) * 60;
      return s;
    };

    let best = 0;
    let bestScore = Infinity;
    for (let i = 0; i < cand.length; i++) {
      const s = score(cand[i]);
      if (s < bestScore - 0.5) {
        best = i;
        bestScore = s;
      }
      if (s === 0) break;
    }
    const keep = it.prevSlot;
    if (
      keep != null &&
      keep >= 0 &&
      keep < cand.length &&
      keep !== best &&
      keep - best < 3 &&
      score(cand[keep]) <= bestScore
    ) {
      best = keep;
    }
    const chosen = cand[best];
    placed.push(chosen);
    result.set(it.key, { key: it.key, chip: chosen, slot: best });
  }

  return result;
}

/**
 * Pure hit-testing from a CSS pixel point `(x, y)` and the drawn crafts to a craft key or null.
 * Resolves to the craft whose center is nearest `(x, y)` within `maxRadius` (breaking ties by
 * larger `pose.z`, i.e. nearer the viewer).
 */
export function hitTestCraft(
  x: number,
  y: number,
  crafts: ReadonlyArray<{ key: string; px: number; py: number; rad: number; pose: { z: number } }>,
  maxRadius = 26,
): string | null {
  let bestKey: string | null = null;
  let bestDist = Infinity;
  let bestZ = -Infinity;

  for (const c of crafts) {
    const hitRadius = Math.max(maxRadius, c.rad + 6);
    const d = Math.hypot(c.px - x, c.py - y);
    if (d > hitRadius) continue;
    if (d < bestDist - 1e-6 || (Math.abs(d - bestDist) <= 1e-6 && c.pose.z > bestZ)) {
      bestDist = d;
      bestZ = c.pose.z;
      bestKey = c.key;
    }
  }

  return bestKey;
}

/**
 * Pure frame builder: evaluates `timeline` and `poseAt` for every craft in `model` at `nowMs`,
 * computes trails (only for moving phases) and landing site flags, and places non-overlapping labels.
 */
export function prepareBoardFrame(
  model: FleetModel,
  view: BoardView,
  G: BoardGeom,
  nowMs: number,
  selectedKey: string | null = null,
  opts?: {
    wallSec?: number;
    still?: boolean;
    extraObstacles?: ChipBox[];
    prevSlots?: ReadonlyMap<string, number>;
  },
): BoardFrame {
  const tSec = nowMs / 1000;
  const wall = opts?.wallSec ?? tSec;
  const still = opts?.still ?? false;

  const crafts: DrawnCraft[] = [];
  const landedSites = new Set<number>();

  for (const fc of model.crafts) {
    const tl = timeline(G, fc.craft);

    if (fc.stage === 'landed' && fc.craft.site != null) {
      if (tl.touch == null || tSec >= tl.touch) {
        landedSites.add(mod(fc.craft.site, G.sites.length));
      }
    }

    const pose = poseAt(G, fc.craft, tl, tSec);
    if (!pose) continue;

    const H = extent(pose);
    const lift = pose.kind === 'stack' ? clamp01(pose.mid / PROFILE.MID.stack) : pose.anchor;
    const off = (H / 2) * (1 - lift);
    const [px, py] = toPx(
      view,
      G,
      pose.x + pose.dx * off,
      pose.y + pose.dy * off,
      pose.z + pose.dz * off,
    );
    const ph = pose.phase;
    const ground = ph === 'pad' || ph === 'landed' || ph === 'splash';
    const between = ph === 'ascent' || ph === 'coast' || ph === 'descent' || ph === 'free-return';
    const isMoving = MOVING_PHASES.has(ph);
    const up = G.project(pose.dx, pose.dy, pose.dz);
    const selected = selectedKey === fc.key;

    crafts.push({
      key: fc.key,
      id: fc.id,
      label: fc.label,
      chipPhrase: fc.chipPhrase,
      fleetCraft: fc,
      tl,
      pose,
      wait: fc.ring,
      px,
      py,
      rad: Math.max(9, H * view.k * 0.5),
      ground,
      nx: up[0],
      ny: -up[1],
      selected,
      trail: isMoving ? trail(G, fc.craft, tl, tSec, 16, 0.13) : null,
      ahead: between ? trail(G, fc.craft, tl, tSec, 40, -0.45) : null,
      chip: null,
    });
  }

  const flags: Point2[] = [];
  const sortedSites = [...landedSites].sort((a, b) => a - b);
  for (const siteIdx of sortedSites) {
    const site = G.sites[siteIdx];
    const g = G.surface(G.M, site.th, site.z, 0.05);
    flags.push(toPx(view, G, g.pos[0], g.pos[1], g.pos[2]));
  }

  // Place non-overlapping labels for visible crafts (except liftoff unless selected)
  const labelInputs: LabelCandidateInput[] = [];
  for (const cr of crafts) {
    if (cr.pose.phase === 'liftoff' && !cr.selected) continue;
    const textLen = Math.max(cr.label.length, cr.chipPhrase.length, 6);
    const w = Math.min(180, Math.max(64, textLen * 6.5 + 16));
    const h = cr.chipPhrase ? 30 : 20;
    labelInputs.push({
      key: cr.key,
      px: cr.px,
      py: cr.py,
      rad: cr.rad,
      ground: cr.ground,
      nx: cr.nx,
      ny: cr.ny,
      w,
      h,
      rank: cr.wait ? 0 : cr.selected ? 1 : 2,
      prevSlot: opts?.prevSlots?.get(cr.key),
    });
  }

  const obstacles = boardObstacleBoxes(view, G, opts?.extraObstacles);
  const placed = placeLabels(labelInputs, obstacles, view);
  const slots = new Map<string, number>();
  for (const cr of crafts) {
    const p = placed.get(cr.key);
    if (p) {
      cr.chip = p.chip;
      slots.set(cr.key, p.slot);
    }
  }

  return {
    G,
    view,
    crafts,
    flags,
    slots,
    wall,
    still,
  };
}

/** Minimal 2D context subset used by `createBoardPlotter`, allowing a lightweight test recorder. */
export interface PlotContext2D {
  fillStyle: string | CanvasGradient | CanvasPattern;
  strokeStyle: string | CanvasGradient | CanvasPattern;
  lineWidth: number;
  lineCap: CanvasLineCap;
  lineJoin: CanvasLineJoin;
  globalAlpha: number;
  globalCompositeOperation: GlobalCompositeOperation;
  setTransform(a: number, b: number, c: number, d: number, e: number, f: number): void;
  clearRect(x: number, y: number, w: number, h: number): void;
  beginPath(): void;
  closePath(): void;
  moveTo(x: number, y: number): void;
  lineTo(x: number, y: number): void;
  arc(x: number, y: number, radius: number, startAngle: number, endAngle: number): void;
  ellipse(
    x: number,
    y: number,
    radiusX: number,
    radiusY: number,
    rotation: number,
    startAngle: number,
    endAngle: number,
  ): void;
  fill(fillRule?: CanvasFillRule): void;
  stroke(): void;
  fillRect(x: number, y: number, w: number, h: number): void;
  setLineDash(segments: number[]): void;
  createRadialGradient(
    x0: number,
    y0: number,
    r0: number,
    x1: number,
    y1: number,
    r1: number,
  ): CanvasGradient;
  drawImage(image: CanvasImageSource, dx: number, dy: number, dw: number, dh: number): void;
  save(): void;
  restore(): void;
  translate(x: number, y: number): void;
  rotate(angle: number): void;
}

export interface LayerCanvas {
  width: number;
  height: number;
  getContext(contextId: '2d'): PlotContext2D | null;
}

export interface BoardPlotter {
  setView(view: BoardView): void;
  setColors(colors: Partial<BoardColors>): void;
  draw(frame: BoardFrame): void;
  getRouteBuildCount(): number;
}

function defaultLayerFactory(w: number, h: number): LayerCanvas {
  const cv = document.createElement('canvas');
  cv.width = w;
  cv.height = h;
  return cv as unknown as LayerCanvas;
}

export function createBoardPlotter(
  ctx: PlotContext2D,
  canvas?: { width: number; height: number },
  createLayer: (w: number, h: number) => LayerCanvas = defaultLayerFactory,
): BoardPlotter {
  let V: BoardView = { w: 800, h: 500, dpr: 1, k: 1, cx: 0, cy: 0, layout: 'wide' };
  let col: BoardColors = { ...DEFAULT_BOARD_COLORS };
  let track: CanvasImageSource | null = null;
  let outline: CanvasImageSource | null = null;
  let routeKey = '';
  let routeBuildCount = 0;

  const sx = (X: number): number => V.w / 2 + (X - V.cx) * V.k;
  const sy = (Y: number): number => V.h / 2 - (Y - V.cy) * V.k;

  function halo(G: BoardGeom, P: BodySpec, inner: number, outer: number, color: string): void {
    const x = sx(P.x);
    const y = sy(P.y * G.cosE);
    const g = ctx.createRadialGradient(x, y, inner * V.k, x, y, outer * V.k);
    g.addColorStop(0, color);
    g.addColorStop(1, 'rgba(0,0,0,0)');
    ctx.fillStyle = g;
    ctx.beginPath();
    ctx.arc(x, y, outer * V.k, 0, TAU);
    ctx.fill();
  }

  function ensureRoute(G: BoardGeom): void {
    const key = [G.layout, V.w, V.h, V.dpr, V.k, col.ink].join('|');
    if (key === routeKey && track !== null && outline !== null) return;
    routeKey = key;
    routeBuildCount += 1;

    const STEPS = 40;
    const strip = (a: [Point2, Point2, Point2, Point2], b: [Point2, Point2, Point2, Point2]) => {
      const pts: Point2[] = [];
      for (let i = 0; i <= STEPS; i++) pts.push(bez(a, i / STEPS));
      for (let i = STEPS; i >= 0; i--) pts.push(bez(b, i / STEPS));
      return (c: PlotContext2D) => {
        c.beginPath();
        pts.forEach((p, i) =>
          i ? c.lineTo(sx(p[0]), sy(p[1] * G.cosE)) : c.moveTo(sx(p[0]), sy(p[1] * G.cosE)),
        );
        c.closePath();
      };
    };

    const annulus = (P: BodySpec, r: [number, number]) => (c: PlotContext2D) => {
      c.beginPath();
      c.ellipse(sx(P.x), sy(P.y * G.cosE), r[1] * V.k, r[1] * V.k * G.cosE, 0, 0, TAU);
      c.ellipse(sx(P.x), sy(P.y * G.cosE), r[0] * V.k, r[0] * V.k * G.cosE, 0, 0, TAU);
    };

    const shapes = [
      annulus(G.E, G.bandE),
      annulus(G.M, G.bandM),
      strip(G.outAt(G.bandE[0], G.bandM[1]), G.outAt(G.bandE[1], G.bandM[0])),
      strip(G.backAt(G.bandM[1], G.bandE[0]), G.backAt(G.bandM[0], G.bandE[1])),
    ];

    const pxW = Math.max(1, Math.round(V.w * V.dpr));
    const pxH = Math.max(1, Math.round(V.h * V.dpr));

    const buildLayer = (paint: (c: PlotContext2D) => void): CanvasImageSource => {
      const cv = createLayer(pxW, pxH);
      const c = cv.getContext('2d');
      if (c) {
        c.setTransform(V.dpr, 0, 0, V.dpr, 0, 0);
        c.fillStyle = col.ink;
        c.strokeStyle = col.ink;
        c.lineJoin = 'round';
        paint(c);
      }
      return cv as unknown as CanvasImageSource;
    };

    track = buildLayer((c) => {
      for (const sh of shapes) {
        sh(c);
        c.fill('evenodd');
      }
    });

    outline = buildLayer((c) => {
      c.lineWidth = 2.4;
      for (const sh of shapes) {
        sh(c);
        c.fill('evenodd');
        c.stroke();
      }
      c.globalCompositeOperation = 'destination-out';
      for (const sh of shapes) {
        sh(c);
        c.fill('evenodd');
      }
      c.globalCompositeOperation = 'source-over';
      c.lineWidth = 1.4;
      c.lineCap = 'round';
      const mid = (G.N - 1) / 2;
      for (const leg of [G.outAt(G.rE(mid), G.rM(mid)), G.backAt(G.rM(mid), G.rE(mid))]) {
        for (const u of [0.3, 0.5, 0.7]) {
          const p = bez(leg, u);
          const q = bez(leg, u + 0.01);
          const x = sx(p[0]);
          const y = sy(p[1] * G.cosE);
          const a = Math.atan2(sy(q[1] * G.cosE) - y, sx(q[0]) - x);
          c.beginPath();
          c.moveTo(x + Math.cos(a + 2.5) * 6, y + Math.sin(a + 2.5) * 6);
          c.lineTo(x, y);
          c.lineTo(x + Math.cos(a - 2.5) * 6, y + Math.sin(a - 2.5) * 6);
          c.stroke();
        }
      }
    });
  }

  function strokePath(G: BoardGeom, pts: number[], from: number): void {
    ctx.beginPath();
    for (let i = from; i < pts.length / 3; i++) {
      const p = G.project(pts[i * 3], pts[i * 3 + 1], pts[i * 3 + 2]);
      if (i === from) ctx.moveTo(sx(p[0]), sy(p[1]));
      else ctx.lineTo(sx(p[0]), sy(p[1]));
    }
    ctx.stroke();
  }

  return {
    setView(view: BoardView): void {
      V = { ...view };
      if (canvas) {
        const nextW = Math.max(1, Math.round(view.w * view.dpr));
        const nextH = Math.max(1, Math.round(view.h * view.dpr));
        if (canvas.width !== nextW || canvas.height !== nextH) {
          canvas.width = nextW;
          canvas.height = nextH;
        }
      }
    },

    setColors(colors: Partial<BoardColors>): void {
      col = { ...col, ...colors };
    },

    getRouteBuildCount(): number {
      return routeBuildCount;
    },

    draw(f: BoardFrame): void {
      const G = f.G;
      const E = G.E;
      const M = G.M;
      ctx.setTransform(V.dpr, 0, 0, V.dpr, 0, 0);
      ctx.clearRect(0, 0, V.w, V.h);
      ctx.lineCap = 'round';
      ctx.lineJoin = 'round';

      halo(G, E, E.R * 0.96, E.R * 1.42, col.earthGlow);
      halo(G, M, M.R * 0.96, M.R * 1.5, col.moonGlow);
      ensureRoute(G);
      if (track && outline) {
        ctx.globalAlpha = col.trackAlpha;
        ctx.drawImage(track, 0, 0, V.w, V.h);
        ctx.globalAlpha = col.outlineAlpha;
        ctx.drawImage(outline, 0, 0, V.w, V.h);
        ctx.globalAlpha = 1;
      }

      // 2D stand-in bodies (Earth and Moon)
      const bodies: Array<[BodySpec, string, string]> = [
        [E, '#4f9fdc', '#2a6fb0'],
        [M, '#d6d7da', '#8f949d'],
      ];
      for (const [P, a, b] of bodies) {
        const x = sx(P.x);
        const y = sy(P.y * G.cosE);
        const r = P.R * V.k;
        const g = ctx.createRadialGradient(x - r * 0.35, y - r * 0.4, r * 0.1, x, y, r);
        g.addColorStop(0, a);
        g.addColorStop(1, b);
        ctx.fillStyle = g;
        ctx.beginPath();
        ctx.arc(x, y, r, 0, TAU);
        ctx.fill();
      }

      // Flags where craft have landed
      ctx.fillStyle = col.flag;
      ctx.strokeStyle = col.ink;
      ctx.lineWidth = 1;
      for (const s of f.flags) {
        ctx.beginPath();
        ctx.moveTo(s[0], s[1]);
        ctx.lineTo(s[0], s[1] - 11);
        ctx.stroke();
        ctx.fillRect(s[0], s[1] - 11, 7, 4.5);
      }

      // Dotted course ahead & trails (only for moving phases)
      for (const c of f.crafts) {
        if (c.ahead && c.ahead.length >= 6) {
          ctx.strokeStyle = c.pose.dead ? col.dim : col.line2;
          ctx.lineWidth = 1.2;
          ctx.setLineDash([1, 6]);
          strokePath(G, c.ahead, 0);
          ctx.setLineDash([]);
        }
        const t = c.trail;
        if (!t || t.length < 6 || !MOVING_PHASES.has(c.pose.phase)) continue;
        const n = t.length / 3;
        ctx.strokeStyle = c.pose.burn > 0.3 ? col.burn : col.ink;
        for (let i = 0; i < n - 1; i++) {
          const p = G.project(t[i * 3], t[i * 3 + 1], t[i * 3 + 2]);
          const q = G.project(t[i * 3 + 3], t[i * 3 + 4], t[i * 3 + 5]);
          const u = 1 - i / (n - 1);
          ctx.globalAlpha = 0.5 * u * u * c.pose.grow;
          ctx.lineWidth = 1 + 2.2 * u;
          ctx.beginPath();
          ctx.moveTo(sx(p[0]), sy(p[1]));
          ctx.lineTo(sx(q[0]), sy(q[1]));
          ctx.stroke();
        }
        ctx.globalAlpha = 1;
      }

      // Craft rings, leaders, and directional markers
      for (const c of f.crafts) {
        const x = c.px;
        const y = c.py;
        if (c.pose.phase === 'splash' && !f.still) {
          ctx.strokeStyle = col.ink;
          ctx.lineWidth = 1;
          for (let i = 0; i < 2; i++) {
            const u = mod(f.wall * 0.5 + i * 0.5, 1);
            ctx.globalAlpha = 0.5 * (1 - u) * c.pose.grow;
            ctx.beginPath();
            ctx.ellipse(x, y + 2, 5 + u * 16, (5 + u * 16) * 0.4, 0, 0, TAU);
            ctx.stroke();
          }
          ctx.globalAlpha = 1;
        }
        if (c.wait) {
          // The one signal on the board: this craft is waiting on you.
          ctx.strokeStyle = col.hold;
          ctx.lineWidth = 2;
          ctx.globalAlpha = 0.95;
          ctx.beginPath();
          ctx.arc(x, y, c.rad + 5, 0, TAU);
          if (!f.still) {
            const u = mod(f.wall + c.px * 0.01, 1.6) / 1.6;
            const rPulse = c.rad + 5 + u * 14;
            ctx.moveTo(x + rPulse, y);
            ctx.arc(x, y, rPulse, 0, TAU);
          }
          ctx.stroke();
          ctx.globalAlpha = 1;
        }
        if (c.selected) {
          ctx.strokeStyle = col.ink;
          ctx.lineWidth = 1.4;
          ctx.setLineDash([3, 4]);
          ctx.beginPath();
          ctx.arc(x, y, c.rad + 10, 0, TAU);
          ctx.stroke();
          ctx.setLineDash([]);
        }
        if (c.chip) {
          const r = c.chip;
          const tx = Math.max(r.x, Math.min(x, r.x + r.w));
          const ty = Math.max(r.y, Math.min(y, r.y + r.h));
          const d = Math.hypot(tx - x, ty - y);
          if (d > c.rad + 3) {
            const ux = (tx - x) / d;
            const uy = (ty - y) / d;
            ctx.strokeStyle = col.line2;
            ctx.lineWidth = 1;
            ctx.beginPath();
            ctx.moveTo(x + ux * (c.rad + 1), y + uy * (c.rad + 1));
            ctx.lineTo(tx, ty);
            ctx.stroke();
          }
        }

        // Stand-in craft triangle pointing along its nose
        const p = c.pose;
        const a = Math.atan2(-p.dy * G.cosE, p.dx);
        ctx.save();
        ctx.translate(x, y);
        ctx.rotate(a);
        ctx.fillStyle = p.dead ? col.dim : col.ink;
        ctx.beginPath();
        ctx.moveTo(9, 0);
        ctx.lineTo(-6, 4.5);
        ctx.lineTo(-6, -4.5);
        ctx.closePath();
        ctx.fill();
        ctx.restore();
      }
    },
  };
}
