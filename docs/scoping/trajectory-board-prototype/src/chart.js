/* chart.js: the plot board under the models. The figure-8 route (Earth
 * orbit, the crossing to the Moon, lunar orbit, the crossing back), each
 * craft's trail and plotted course, and the red ring round anything that is
 * waiting on you, drawn on a 2D canvas with the same projection the 3D
 * camera uses. If WebGL is not available it also draws stand-in bodies and
 * craft, so the board still reads.
 */
(function (KB) {
  'use strict';

  const TAU = Math.PI * 2;
  const mod = (a, n) => ((a % n) + n) % n;

  function createChart(canvas) {
    const ctx = canvas.getContext('2d');
    let V = { w: 1, h: 1, dpr: 1, k: 1, cx: 0, cy: 0 };
    let col = {};
    const api = {};

    api.setView = function (w, h, dpr, k, cx, cy) {
      V = { w, h, dpr, k, cx, cy };
      canvas.width = Math.round(w * dpr);
      canvas.height = Math.round(h * dpr);
    };
    api.setColors = function (c) {
      col = c;
    };

    // world -> CSS pixels
    const sx = (X) => V.w / 2 + (X - V.cx) * V.k;
    const sy = (Y) => V.h / 2 - (Y - V.cy) * V.k;
    api.toPx = function (G, x, y, z) {
      const p = G.project(x, y, z);
      return [sx(p[0]), sy(p[1])];
    };

    function halo(G, P, inner, outer, color) {
      const x = sx(P.x), y = sy(P.y * G.cosE);
      const g = ctx.createRadialGradient(x, y, inner * V.k, x, y, outer * V.k);
      g.addColorStop(0, color);
      g.addColorStop(1, 'rgba(0,0,0,0)');
      ctx.fillStyle = g;
      ctx.beginPath();
      ctx.arc(x, y, outer * V.k, 0, TAU);
      ctx.fill();
    }

    /* The route, as one track: the two orbit bands and the two crossings
     * between them. It only changes with the view, so it is drawn once into
     * two layers (the track, and the outline of the whole shape) and reused. */
    let track = null, outline = null, routeKey = '';
    function route(G) {
      const key = [G.layout, V.w, V.h, V.dpr, V.k, col.ink].join('|');
      if (key === routeKey) return;
      routeKey = key;
      const N = G.N, STEPS = 40;
      const strip = (a, b) => {
        // The band between two edge curves, as a closed outline in projected pixels.
        const pts = [];
        for (let i = 0; i <= STEPS; i++) pts.push(KB.bez(a, i / STEPS));
        for (let i = STEPS; i >= 0; i--) pts.push(KB.bez(b, i / STEPS));
        return (c) => {
          c.beginPath();
          pts.forEach((p, i) => (i ? c.lineTo(sx(p[0]), sy(p[1] * G.cosE)) : c.moveTo(sx(p[0]), sy(p[1] * G.cosE))));
          c.closePath();
        };
      };
      const annulus = (P, r) => (c) => {
        c.beginPath();
        c.ellipse(sx(P.x), sy(P.y * G.cosE), r[1] * V.k, r[1] * V.k * G.cosE, 0, 0, TAU);
        c.ellipse(sx(P.x), sy(P.y * G.cosE), r[0] * V.k, r[0] * V.k * G.cosE, 0, 0, TAU);
      };
      const shapes = [
        annulus(G.E, G.bandE), annulus(G.M, G.bandM),
        strip(G.outAt(G.bandE[0], G.bandM[1]), G.outAt(G.bandE[1], G.bandM[0])),
        strip(G.backAt(G.bandM[1], G.bandE[0]), G.backAt(G.bandM[0], G.bandE[1])),
      ];
      const layer = (paint) => {
        const cv = document.createElement('canvas');
        cv.width = canvas.width;
        cv.height = canvas.height;
        const c = cv.getContext('2d');
        c.setTransform(V.dpr, 0, 0, V.dpr, 0, 0);
        c.fillStyle = c.strokeStyle = col.ink;
        c.lineJoin = 'round';
        paint(c);
        return cv;
      };
      track = layer((c) => {
        for (const sh of shapes) { sh(c); c.fill('evenodd'); }
      });
      // Outline of the union: every shape drawn a little fat, then every shape's inside taken away.
      outline = layer((c) => {
        c.lineWidth = 2.4;
        for (const sh of shapes) { sh(c); c.fill('evenodd'); c.stroke(); }
        c.globalCompositeOperation = 'destination-out';
        for (const sh of shapes) { sh(c); c.fill('evenodd'); }
        // Chevrons down the middle of each crossing: which way it runs.
        c.globalCompositeOperation = 'source-over';
        c.lineWidth = 1.4;
        c.lineCap = 'round';
        const mid = (G.N - 1) / 2;
        for (const leg of [G.outAt(G.rE(mid), G.rM(mid)), G.backAt(G.rM(mid), G.rE(mid))]) {
          for (const u of [0.3, 0.5, 0.7]) {
            const p = KB.bez(leg, u), q = KB.bez(leg, u + 0.01);
            const x = sx(p[0]), y = sy(p[1] * G.cosE), a = Math.atan2(sy(q[1] * G.cosE) - y, sx(q[0]) - x);
            c.beginPath();
            c.moveTo(x + Math.cos(a + 2.5) * 6, y + Math.sin(a + 2.5) * 6);
            c.lineTo(x, y);
            c.lineTo(x + Math.cos(a - 2.5) * 6, y + Math.sin(a - 2.5) * 6);
            c.stroke();
          }
        }
      });
    }
    function path(G, pts, from) {
      ctx.beginPath();
      for (let i = from; i < pts.length / 3; i++) {
        const p = G.project(pts[i * 3], pts[i * 3 + 1], pts[i * 3 + 2]);
        if (i === from) ctx.moveTo(sx(p[0]), sy(p[1]));
        else ctx.lineTo(sx(p[0]), sy(p[1]));
      }
      ctx.stroke();
    }

    api.draw = function (f) {
      const G = f.G, E = G.E, M = G.M;
      ctx.setTransform(V.dpr, 0, 0, V.dpr, 0, 0);
      ctx.clearRect(0, 0, V.w, V.h);
      ctx.lineCap = 'round';
      ctx.lineJoin = 'round';

      halo(G, E, E.R * 0.96, E.R * 1.42, col.earthGlow);
      halo(G, M, M.R * 0.96, M.R * 1.5, col.moonGlow);
      route(G);
      ctx.globalAlpha = col.trackAlpha;
      ctx.drawImage(track, 0, 0, V.w, V.h);
      ctx.globalAlpha = col.outlineAlpha;
      ctx.drawImage(outline, 0, 0, V.w, V.h);
      ctx.globalAlpha = 1;

      if (f.noGL) {
        for (const [P, a, b] of [[E, '#4f9fdc', '#2a6fb0'], [M, '#d6d7da', '#8f949d']]) {
          const x = sx(P.x), y = sy(P.y * G.cosE), r = P.R * V.k;
          const g = ctx.createRadialGradient(x - r * 0.35, y - r * 0.4, r * 0.1, x, y, r);
          g.addColorStop(0, a);
          g.addColorStop(1, b);
          ctx.fillStyle = g;
          ctx.beginPath();
          ctx.arc(x, y, r, 0, TAU);
          ctx.fill();
        }
        // Flags where craft have landed.
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
      }

      for (const c of f.crafts) {
        // The course ahead, dotted, while the craft is between places.
        if (c.ahead && c.ahead.length >= 6) {
          ctx.strokeStyle = c.pose.dead ? col.dim : col.line2;
          ctx.lineWidth = 1.2;
          ctx.setLineDash([1, 6]);
          path(G, c.ahead, 0);
          ctx.setLineDash([]);
        }
        // The trail: where it has just been.
        const t = c.trail;
        if (!t || t.length < 6) continue;
        const n = t.length / 3;
        ctx.strokeStyle = c.pose.burn > 0.3 ? col.burn : col.ink;
        for (let i = 0; i < n - 1; i++) {
          const p = G.project(t[i * 3], t[i * 3 + 1], t[i * 3 + 2]), q = G.project(t[i * 3 + 3], t[i * 3 + 4], t[i * 3 + 5]);
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

      for (const c of f.crafts) {
        const x = c.px, y = c.py;
        if (c.pose.phase === 'splash' && !f.still) {
          // Ripples where a failed mission came down.
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
          ctx.stroke();
          if (!f.still) {
            const u = mod(f.wall + c.px * 0.01, 1.6) / 1.6;
            ctx.globalAlpha = 0.7 * (1 - u) * (1 - u);
            ctx.beginPath();
            ctx.arc(x, y, c.rad + 5 + u * 18, 0, TAU);
            ctx.stroke();
          }
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
          // Leader from the craft to the nearest edge of its label.
          const r = c.chip, tx = Math.max(r.x, Math.min(x, r.x + r.w)), ty = Math.max(r.y, Math.min(y, r.y + r.h));
          const d = Math.hypot(tx - x, ty - y);
          if (d > c.rad + 3) {
            const ux = (tx - x) / d, uy = (ty - y) / d;
            ctx.strokeStyle = c.wait ? col.hold : col.line2;
            ctx.lineWidth = 1;
            ctx.beginPath();
            ctx.moveTo(x + ux * (c.rad + 1), y + uy * (c.rad + 1));
            ctx.lineTo(tx, ty);
            ctx.stroke();
          }
        }
        if (f.noGL) {
          const p = c.pose, a = Math.atan2(-p.dy * G.cosE, p.dx);
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
      }
    };

    return api;
  }

  KB.createChart = createChart;
})((globalThis.KB = globalThis.KB || {}));
