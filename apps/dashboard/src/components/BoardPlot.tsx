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
  type DrawnCraft,
  type PlotContext2D,
} from '../lib/board/plot';
import { visible } from '../lib/presentation';

export interface BoardPlotProps {
  model: FleetModel;
  selectedKey: string | null;
  onSelectCraft: (craftKey: string | null) => void;
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

  const [supported, setSupported] = useState<boolean | null>(null);
  const [drawnCrafts, setDrawnCrafts] = useState<DrawnCraft[]>([]);
  const [viewportVersion, setViewportVersion] = useState(0);

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

  // Observe stage resize when supported
  useEffect(() => {
    if (supported !== true) return;
    const el = stageRef.current;
    if (!el) return;

    const measure = () => {
      const w = el.clientWidth || 800;
      const h = el.clientHeight || 500;
      if (w !== sizeRef.current.w || h !== sizeRef.current.h) {
        sizeRef.current = { w, h };
        setViewportVersion((v) => v + 1);
      }
    };
    measure();

    if (typeof ResizeObserver !== 'undefined') {
      const ro = new ResizeObserver(measure);
      ro.observe(el);
      return () => ro.disconnect();
    }
    return undefined;
  }, [supported]);

  // Render frame and manage animation loop (paused when document.hidden or prefers-reduced-motion)
  useEffect(() => {
    if (supported !== true) return;
    const plotter = plotterRef.current;
    if (!plotter) return;

    const still = readReducedMotion();
    const dpr = typeof window !== 'undefined' ? window.devicePixelRatio || 1 : 1;
    const { w, h } = sizeRef.current;
    const { view, G } = fitBoard(w, h, dpr);
    plotter.setColors(readPlotColors(stageRef.current));
    plotter.setView(view);

    const baseWallMs = typeof performance !== 'undefined' ? performance.now() : 0;
    const baseModelMs = model.nowMs;

    const drawAt = (elapsedMs: number) => {
      const currentMs = still ? baseModelMs : baseModelMs + elapsedMs;
      const wallSec = (baseModelMs + elapsedMs) / 1000;
      const frame = prepareBoardFrame(model, view, G, currentMs, selectedKey, {
        wallSec,
        still,
        extraObstacles: [{ x: 14, y: 12, w: 170, h: 20 }],
      });
      plotter.draw(frame);
      drawnRef.current = frame.crafts;
      setDrawnCrafts(frame.crafts);
    };

    // Always draw once immediately for the current model/selection/size
    drawAt(0);

    if (still || typeof requestAnimationFrame !== 'function') {
      return undefined;
    }

    let rafId = 0;
    let active = true;

    const tick = (nowPerf: number) => {
      if (!active) return;
      if (typeof document !== 'undefined' && document.hidden) {
        rafId = 0;
        return;
      }
      drawAt(Math.max(0, nowPerf - baseWallMs));
      rafId = requestAnimationFrame(tick);
    };

    const startLoop = () => {
      if (!active || rafId !== 0) return;
      if (typeof document !== 'undefined' && document.hidden) return;
      rafId = requestAnimationFrame(tick);
    };

    const onVisibility = () => {
      if (typeof document !== 'undefined' && document.hidden) {
        if (rafId !== 0 && typeof cancelAnimationFrame === 'function') {
          cancelAnimationFrame(rafId);
          rafId = 0;
        }
      } else {
        startLoop();
      }
    };

    startLoop();
    if (typeof document !== 'undefined') {
      document.addEventListener('visibilitychange', onVisibility);
    }

    return () => {
      active = false;
      if (rafId !== 0 && typeof cancelAnimationFrame === 'function') {
        cancelAnimationFrame(rafId);
      }
      if (typeof document !== 'undefined') {
        document.removeEventListener('visibilitychange', onVisibility);
      }
    };
  }, [supported, model, selectedKey, viewportVersion]);

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
