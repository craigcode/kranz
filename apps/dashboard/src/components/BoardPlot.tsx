// 2D canvas + positioned craft chip layer for the Trajectory Board lens —
// docs/scoping/trajectory-board.md (slice 4).
// Degrades cleanly to nothing when `canvas.getContext('2d')` is unavailable (e.g. jsdom).

import { useEffect, useRef, useState } from 'react';
import type { FleetModel } from '../lib/board/fleet';
import {
  createBoardPlotter,
  DEFAULT_BOARD_COLORS,
  fitBoard,
  hitTestCraft,
  prepareBoardFrame,
  type BoardColors,
  type BoardPlotter,
  type BoardView,
  type DrawnCraft,
  type PlotContext2D,
} from '../lib/board/plot';
import type { BoardGeom } from '../lib/board/flightPlan';
import { visible } from '../lib/presentation';

export interface BoardPlotProps {
  model: FleetModel;
  selectedKey: string | null;
  onSelectCraft: (craftKey: string | null) => void;
}

const MAX_TRANSITION_MS = 75_000;

function hasActiveMotion(model: FleetModel, currentMs: number, transitionEndMs: number): boolean {
  if (model.crafts.some((c) => c.stage === 'running')) return true;
  if (model.transitions.length > 0) return true;
  return currentMs < transitionEndMs;
}

function chipTone(cr: DrawnCraft): string {
  if (cr.wait) return 'wait';
  if (cr.pose.dead) return 'dead';
  if (cr.pose.phase === 'landed') return 'landed';
  return '';
}

function sameRenderedChips(prev: DrawnCraft[], next: DrawnCraft[]): boolean {
  if (prev.length !== next.length) return false;
  for (let i = 0; i < prev.length; i++) {
    const a = prev[i];
    const b = next[i];
    if (
      a.key !== b.key ||
      a.selected !== b.selected ||
      a.label !== b.label ||
      a.chipPhrase !== b.chipPhrase ||
      chipTone(a) !== chipTone(b)
    ) {
      return false;
    }
    if ((a.chip === null) !== (b.chip === null)) return false;
    if (
      a.chip !== null &&
      b.chip !== null &&
      (Math.round(a.chip.x) !== Math.round(b.chip.x) ||
        Math.round(a.chip.y) !== Math.round(b.chip.y))
    ) {
      return false;
    }
  }
  return true;
}

function acquireContext2D(canvas: HTMLCanvasElement): PlotContext2D | null {
  const origError = console.error;
  try {
    console.error = () => {};
    return (canvas.getContext('2d') as unknown as PlotContext2D | null) ?? null;
  } catch {
    return null;
  } finally {
    console.error = origError;
  }
}

function readReducedMotion(): boolean {
  if (typeof window === 'undefined' || typeof window.matchMedia !== 'function') return false;
  try {
    return Boolean(window.matchMedia('(prefers-reduced-motion: reduce)')?.matches);
  } catch {
    return false;
  }
}

function readPlotColors(el: HTMLElement | null): Partial<BoardColors> {
  if (!el || typeof window === 'undefined' || typeof window.getComputedStyle !== 'function') {
    return DEFAULT_BOARD_COLORS;
  }
  const cs = window.getComputedStyle(el);
  const v = (name: string, fallback: string): string => {
    const val = cs.getPropertyValue(name).trim();
    return val !== '' ? val : fallback;
  };
  const num = (name: string, fallback: number): number => {
    const val = Number(cs.getPropertyValue(name).trim());
    return Number.isFinite(val) && val > 0 ? val : fallback;
  };
  return {
    ink: v('--board-ink', DEFAULT_BOARD_COLORS.ink),
    dim: v('--board-ink-dim', DEFAULT_BOARD_COLORS.dim),
    line: v('--board-ink-line', DEFAULT_BOARD_COLORS.line),
    line2: v('--board-ink-line2', DEFAULT_BOARD_COLORS.line2),
    hold: v('--board-hold', DEFAULT_BOARD_COLORS.hold),
    trackAlpha: num('--board-track', DEFAULT_BOARD_COLORS.trackAlpha),
    outlineAlpha: num('--board-track-edge', DEFAULT_BOARD_COLORS.outlineAlpha),
    burn: v('--board-burn', DEFAULT_BOARD_COLORS.burn),
    earthGlow: v('--board-earth-glow', DEFAULT_BOARD_COLORS.earthGlow),
    moonGlow: v('--board-moon-glow', DEFAULT_BOARD_COLORS.moonGlow),
    flag: v('--board-flag', DEFAULT_BOARD_COLORS.flag),
  };
}

export function BoardPlot({ model, selectedKey, onSelectCraft }: BoardPlotProps) {
  const stageRef = useRef<HTMLDivElement | null>(null);
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const plotterRef = useRef<BoardPlotter | null>(null);
  const drawnRef = useRef<DrawnCraft[]>([]);
  const sizeRef = useRef<{ w: number; h: number }>({ w: 800, h: 500 });
  const prevSlotsRef = useRef<Map<string, number>>(new Map());
  const modelRef = useRef<FleetModel>(model);
  const selectedKeyRef = useRef<string | null>(selectedKey);
  const anchorRef = useRef<{ modelNowMs: number; perfMs: number }>({
    modelNowMs: model.nowMs,
    perfMs: typeof performance !== 'undefined' ? performance.now() : 0,
  });
  const transitionEndMsRef = useRef<number>(-Infinity);
  const syncRef = useRef<(() => void) | null>(null);

  const [supported, setSupported] = useState<boolean | null>(null);
  const [drawnCrafts, setDrawnCrafts] = useState<DrawnCraft[]>([]);

  // Probe 2D canvas support on mount
  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    const ctx = acquireContext2D(canvas);
    if (!ctx) {
      setSupported(false);
      return;
    }
    const plotter = createBoardPlotter(ctx, canvas);
    plotter.setColors(readPlotColors(stageRef.current));
    plotterRef.current = plotter;
    setSupported(true);
    return () => {
      plotterRef.current = null;
    };
  }, []);

  // Mount ResizeObserver, visibilitychange listener, and frame controller once when supported
  useEffect(() => {
    if (supported !== true) return;
    const el = stageRef.current;
    const plotter = plotterRef.current;
    if (!el || !plotter) return;

    let mounted = true;
    let rafId = 0;
    let cachedFit: { w: number; h: number; dpr: number; view: BoardView; G: BoardGeom } | null =
      null;

    const ensureView = () => {
      const { w, h } = sizeRef.current;
      const dpr = typeof window !== 'undefined' ? window.devicePixelRatio || 1 : 1;
      if (!cachedFit || cachedFit.w !== w || cachedFit.h !== h || cachedFit.dpr !== dpr) {
        const { view, G } = fitBoard(w, h, dpr);
        plotter.setColors(readPlotColors(stageRef.current));
        plotter.setView(view);
        cachedFit = { w, h, dpr, view, G };
      }
      return cachedFit;
    };

    const stopLoop = () => {
      if (rafId !== 0 && typeof cancelAnimationFrame === 'function') {
        cancelAnimationFrame(rafId);
      }
      rafId = 0;
    };

    const drawFrame = (elapsedMs: number, still: boolean): number => {
      const { view, G } = ensureView();
      const curModel = modelRef.current;
      const curSelected = selectedKeyRef.current;
      const baseModelMs = anchorRef.current.modelNowMs;
      const currentMs = still ? baseModelMs : baseModelMs + elapsedMs;
      const wallSec = (baseModelMs + elapsedMs) / 1000;
      const frame = prepareBoardFrame(curModel, view, G, currentMs, curSelected, {
        wallSec,
        still,
        extraObstacles: [{ x: 14, y: 12, w: 170, h: 20 }],
        prevSlots: prevSlotsRef.current,
      });
      prevSlotsRef.current = frame.slots;
      plotter.draw(frame);
      drawnRef.current = frame.crafts;
      setDrawnCrafts((prev) => (sameRenderedChips(prev, frame.crafts) ? prev : frame.crafts));
      return currentMs;
    };

    const tick = (nowPerf: number) => {
      rafId = 0;
      if (!mounted) return;
      if (typeof document !== 'undefined' && document.hidden) return;
      const still = readReducedMotion();
      const elapsed = Math.max(0, nowPerf - anchorRef.current.perfMs);
      const currentMs = drawFrame(elapsed, still);
      if (
        !still &&
        typeof requestAnimationFrame === 'function' &&
        hasActiveMotion(modelRef.current, currentMs, transitionEndMsRef.current)
      ) {
        rafId = requestAnimationFrame(tick);
      }
    };

    const sync = () => {
      if (!mounted) return;
      const still = readReducedMotion();
      const currentMs = drawFrame(0, still);
      const canAnimate =
        !still &&
        typeof requestAnimationFrame === 'function' &&
        !(typeof document !== 'undefined' && document.hidden) &&
        hasActiveMotion(modelRef.current, currentMs, transitionEndMsRef.current);
      if (canAnimate) {
        if (rafId === 0) {
          rafId = requestAnimationFrame(tick);
        }
      } else {
        stopLoop();
      }
    };

    syncRef.current = sync;

    const measure = () => {
      const w = el.clientWidth || 800;
      const h = el.clientHeight || 500;
      if (w !== sizeRef.current.w || h !== sizeRef.current.h) {
        sizeRef.current = { w, h };
        sync();
      }
    };
    sizeRef.current = { w: el.clientWidth || 800, h: el.clientHeight || 500 };
    sync();

    const ro = typeof ResizeObserver !== 'undefined' ? new ResizeObserver(measure) : null;
    ro?.observe(el);

    const onVisibility = () => {
      if (typeof document !== 'undefined' && document.hidden) {
        stopLoop();
      } else {
        anchorRef.current = {
          modelNowMs: modelRef.current.nowMs,
          perfMs: typeof performance !== 'undefined' ? performance.now() : 0,
        };
        sync();
      }
    };

    if (typeof document !== 'undefined') {
      document.addEventListener('visibilitychange', onVisibility);
    }

    return () => {
      mounted = false;
      syncRef.current = null;
      stopLoop();
      ro?.disconnect();
      if (typeof document !== 'undefined') {
        document.removeEventListener('visibilitychange', onVisibility);
      }
    };
  }, [supported]);

  // Synchronize latest model and selection via refs and trigger a single redraw
  useEffect(() => {
    const prevModel = modelRef.current;
    modelRef.current = model;
    selectedKeyRef.current = selectedKey;
    if (model !== prevModel) {
      if (model.crafts.length === 0 || model.nowMs + 1_000 < prevModel.nowMs) {
        transitionEndMsRef.current = -Infinity;
        prevSlotsRef.current = new Map();
      }
      anchorRef.current = {
        modelNowMs: model.nowMs,
        perfMs: typeof performance !== 'undefined' ? performance.now() : 0,
      };
    }
    if (model.transitions.length > 0) {
      for (const tr of model.transitions) {
        transitionEndMsRef.current = Math.max(
          transitionEndMsRef.current,
          tr.at + MAX_TRANSITION_MS,
        );
      }
    }
    syncRef.current?.();
  }, [supported, model, selectedKey]);

  if (supported === false) {
    return null;
  }

  const handleStageClick = (ev: React.MouseEvent<HTMLDivElement>) => {
    const el = stageRef.current;
    if (!el) return;
    const rect = el.getBoundingClientRect();
    const x = ev.clientX - rect.left;
    const y = ev.clientY - rect.top;
    const hit = hitTestCraft(x, y, drawnRef.current);
    onSelectCraft(hit === selectedKey ? null : hit);
  };

  return (
    <div className="trajectory-board-stage" ref={stageRef} onClick={handleStageClick}>
      <canvas ref={canvasRef} className="trajectory-board-plot" aria-hidden="true" />
      {supported === true && (
        <>
          <div className="trajectory-board-chips" role="group" aria-label="Craft on the board">
            {drawnCrafts.map((cr) => {
              if (!cr.chip) return null;
              const tone = cr.wait
                ? 'wait'
                : cr.pose.dead
                  ? 'dead'
                  : cr.pose.phase === 'landed'
                    ? 'landed'
                    : '';
              return (
                <button
                  key={cr.key}
                  type="button"
                  className="trajectory-board-chip"
                  data-craft-key={cr.key}
                  data-tone={tone}
                  aria-pressed={cr.selected}
                  style={{
                    transform: `translate(${Math.round(cr.chip.x)}px, ${Math.round(cr.chip.y)}px)`,
                  }}
                  onClick={(ev) => {
                    ev.stopPropagation();
                    onSelectCraft(cr.selected ? null : cr.key);
                  }}
                >
                  <b>{visible(cr.label)}</b>
                  {cr.chipPhrase !== '' && <i>{visible(cr.chipPhrase)}</i>}
                </button>
              );
            })}
          </div>
          <p className="trajectory-board-legend">
            <i aria-hidden="true" />
            Red ring: waiting on you
          </p>
        </>
      )}
    </div>
  );
}
