/* profile.js: the flight plan. Geometry of the board, and where each craft
 * is at time t as a pure function of its event timestamps, so the live view
 * and the replay scrubber draw from the same code.
 *
 * Four places, in Apollo's order:
 *   the pad        a plan being drafted, then waiting for "go for launch"
 *   Earth orbit    workers building, validators checking
 *   lunar orbit    work complete, waiting for "go for landing" (the merge)
 *   the surface    merged
 * A failed mission never lands: it swings round the far side on a free
 * return and splashes down, as Apollo 13 did.
 *
 * The route is Apollo's figure 8: counter-clockwise round the Earth, across
 * to the Moon, clockwise round it, and back across to the Earth.
 *
 * World units; the board is the z = 0 plane, +z is toward the viewer.
 */
(function (KB) {
  'use strict';

  const TAU = Math.PI * 2, DEG = Math.PI / 180;
  const ASCENT = 8, DESCENT = 9, ASSEMBLE = 1.6;
  const SHED = 2.4; // seconds after the lunar burn that the third stage is left behind
  const STAY = 8, LIFT = 3.2; // the ascent stage waits for the flag, then leaves
  const REENTRY = 5, SPLASH_HOLD = 9, SPLASH_FADE = 2.5, SCRUB_FADE = 2.4;
  const SCALE = 0.5; // model units -> world units
  // Model heights in model units (scene.js builds to these).
  const HEIGHT = { stack: 5.8, upper: 1.85, csmlm: 2.2, lm: 1.0, cm: 0.5 };
  const MID = { stack: 2.9, upper: 4.6 }; // model-space height of the point a flying stack is held by
  const SIZE = { pad: 1, flight: 1.5, lm: 2, cm: 1.6 }; // drawn larger once away from the pad, so a craft in orbit still reads

  const clamp01 = (v) => (v < 0 ? 0 : v > 1 ? 1 : v);
  const smooth = (v) => { v = clamp01(v); return v * v * (3 - 2 * v); };
  const lerp = (a, b, u) => a + (b - a) * u;
  const mod = (a, n) => ((a % n) + n) % n;
  // Unit direction of travel at angle th on a circle; s = 1 counter-clockwise, -1 clockwise.
  const tan = (th, s) => [-s * Math.sin(th), s * Math.cos(th)];

  function makeGeom(layout) {
    const wide = layout !== 'tall';
    const E = wide ? { x: -10.4, y: -4.3, R: 5.4 } : { x: -3.4, y: -11.2, R: 5.4 };
    const M = wide ? { x: 12.6, y: 5.4, R: 3.9 } : { x: 4.4, y: 12.2, R: 3.9 };
    const G = { layout: wide ? 'wide' : 'tall', E, M, N: 5, EL: 18 * DEG };
    G.cosE = Math.cos(G.EL);
    G.sinE = Math.sin(G.EL);
    G.sE = 1; // the Earth is orbited counter-clockwise
    G.sM = -1; // and the Moon clockwise, as Apollo did, which makes the route a figure 8
    G.rE = (i) => E.R + 3.3 + 0.5 * i;
    // Lanes run side by side all the way round the 8, so the inside lane at the Earth is the outside lane at the Moon.
    G.rM = (i) => M.R + 1.6 + 0.42 * (G.N - 1 - i);
    G.wE = (i) => TAU / (22 + 1.6 * i);
    G.wM = (i) => TAU / (15 + 1.3 * (G.N - 1 - i));
    G.bandE = [G.rE(0) - 0.35, G.rE(G.N - 1) + 0.35];
    G.bandM = [G.rM(G.N - 1) - 0.35, G.rM(0) + 0.35];
    const phi = Math.atan2(M.y - E.y, M.x - E.x), dist = Math.hypot(M.x - E.x, M.y - E.y), Q = Math.PI / 2;
    G.phi = phi;
    // The two crossings of the 8 are the crossed tangents between the orbits.
    // Each is left a little early and joined a little late, so it bends rather than running straight.
    const beta = Math.asin(Math.min(0.95, (G.rE(2) + G.rM(2)) / dist)), bend = 16 * DEG;
    G.thD = phi - Q + beta - bend; // leave Earth orbit for the Moon
    G.thA = phi + Q + beta - bend; // lunar orbit insertion
    G.thO = phi - Q - beta + bend; // leave the Moon for home (only a failed mission does)
    G.thR = phi + Q - beta + bend; // back at the Earth
    G.thS = Math.min(G.thR + 55 * DEG, 168 * DEG); // splashdown
    G.joinLead = 50 * DEG;
    G.leadDeo = 58 * DEG; // powered descent starts this far short of the site
    G.pads = [58, 69, 80, 91].map((d) => ({ th: d * DEG, z: 0.35 }));
    // Landing sites: a back row and a front row, filled from the middle out.
    const back = [84, 106, 62, 128, 40, 150], front = [95, 73, 117, 51, 139, 29];
    G.sites = [];
    for (let i = 0; i < 6; i++) G.sites.push({ th: back[i] * DEG, z: -0.45 }, { th: front[i] * DEG, z: 0.95 });
    G.project = (x, y, z) => [x, y * G.cosE - (z || 0) * G.sinE];

    // Point and outward normal on a body's surface at in-plane angle th,
    // pushed z units toward the viewer and lifted h above the ground.
    G.surface = (P, th, z, h) => {
      const s = Math.max(-0.95, Math.min(0.95, (z || 0) / P.R)), c = Math.sqrt(1 - s * s);
      const n = [Math.cos(th) * c, Math.sin(th) * c, s];
      const r = P.R + (h || 0);
      return { pos: [P.x + n[0] * r, P.y + n[1] * r, n[2] * r], n };
    };

    G.ascent = (pad, lane) => {
      const th = G.pads[pad].th, r = G.rE(lane), thJ = th + G.sE * G.joinLead;
      const p0 = [E.x + Math.cos(th) * E.R, E.y + Math.sin(th) * E.R];
      const rise = (r - E.R) * 0.78;
      const p3 = [E.x + Math.cos(thJ) * r, E.y + Math.sin(thJ) * r];
      const tj = tan(thJ, G.sE), k = (G.wE(lane) * r * ASCENT) / 6; // arrive at orbital speed
      return { thJ, c: [p0, [p0[0] + Math.cos(th) * rise, p0[1] + Math.sin(th) * rise], [p3[0] - tj[0] * k, p3[1] - tj[1] * k], p3] };
    };
    // A crossing: from angle a0 on a circle round P0 to angle a1 on a circle round P1, leaving and arriving along the orbits.
    const leg = (P0, r0, a0, s0, P1, r1, a1, s1) => {
      const p0 = [P0.x + Math.cos(a0) * r0, P0.y + Math.sin(a0) * r0], p3 = [P1.x + Math.cos(a1) * r1, P1.y + Math.sin(a1) * r1];
      const k = Math.hypot(p3[0] - p0[0], p3[1] - p0[1]) * 0.34, t0 = tan(a0, s0), t1 = tan(a1, s1);
      return [p0, [p0[0] + t0[0] * k, p0[1] + t0[1] * k], [p3[0] - t1[0] * k, p3[1] - t1[1] * k], p3];
    };
    G.outAt = (rd, ra) => leg(E, rd, G.thD, G.sE, M, ra, G.thA, G.sM); // Earth orbit radius rd to lunar orbit radius ra
    G.backAt = (ra, rd) => leg(M, ra, G.thO, G.sM, E, rd, G.thR, G.sE);
    G.transfer = (lane) => G.outAt(G.rE(lane), G.rM(lane));
    G.homeLeg = (lane) => G.backAt(G.rM(lane), G.rE(lane));
    G.coast = Math.max(6, dist / 2.9); // seconds for either crossing
    G.descent = (lane, site) => {
      const r = G.rM(lane), s = G.sites[site], a0 = s.th - G.sM * G.leadDeo;
      const p0 = [M.x + Math.cos(a0) * r, M.y + Math.sin(a0) * r];
      const t0 = tan(a0, G.sM), k0 = (G.wM(lane) * r * DESCENT) / 6; // leave at orbital speed
      const p3 = [M.x + Math.cos(s.th) * M.R, M.y + Math.sin(s.th) * M.R];
      return [p0, [p0[0] + t0[0] * k0, p0[1] + t0[1] * k0], [p3[0] + Math.cos(s.th) * 1.5, p3[1] + Math.sin(s.th) * 1.5], p3];
    };
    // A failed mission does not brake at the Moon: it swings round the far side and takes the other crossing back.
    G.sweep = mod(G.sM * (G.thO - G.thA), TAU);
    G.swing = (lane) => G.sweep / (G.wM(lane) * 1.4); // seconds
    // From the end of that crossing down through the atmosphere to the sea; u runs 0..1.
    G.reentry = (lane, u) => {
      const r = lerp(G.rE(lane), E.R, smooth(u)), th = lerp(G.thR, G.thS, 1 - (1 - u) * (1 - u));
      return [E.x + Math.cos(th) * r, E.y + Math.sin(th) * r];
    };

    // Board extents in projected units, for framing.
    const pad = 1.3, ro = G.bandE[1], mo = G.bandM[1];
    G.bounds = {
      x0: Math.min(E.x - ro, M.x - mo) - pad, x1: Math.max(E.x + ro, M.x + mo) + pad,
      y0: Math.min(E.y - ro, M.y - mo) * G.cosE - pad, y1: Math.max(E.y + ro, M.y + mo) * G.cosE + pad,
    };
    return G;
  }

  function bez(c, s) {
    const u = 1 - s, a = u * u * u, b = 3 * u * u * s, d = 3 * u * s * s, e = s * s * s;
    return [a * c[0][0] + b * c[1][0] + d * c[2][0] + e * c[3][0], a * c[0][1] + b * c[1][1] + d * c[2][1] + e * c[3][1]];
  }
  function bezTan(c, s) {
    const u = 1 - s;
    const x = 3 * u * u * (c[1][0] - c[0][0]) + 6 * u * s * (c[2][0] - c[1][0]) + 3 * s * s * (c[3][0] - c[2][0]);
    const y = 3 * u * u * (c[1][1] - c[0][1]) + 6 * u * s * (c[2][1] - c[1][1]) + 3 * s * s * (c[3][1] - c[2][1]);
    const l = Math.hypot(x, y) || 1;
    return [x / l, y / l];
  }

  /* Key times for one craft, in seconds. Burns wait for their window: the
   * craft keeps orbiting until it comes round to the right place, then goes. */
  function timeline(G, c) {
    const s = (ms) => (ms == null ? null : ms / 1000);
    const tl = {
      created: s(c.createdAt), padAt: s(c.padAt), assembled: s(c.assembledAt),
      launch: s(c.startedAt), join: null, depart: null, arrive: null, deorbit: null, touch: null,
      fail: s(c.failedAt), abandon: s(c.abandonedAt), home: null, behind: null, back: null, reenter: null, splash: null,
    };
    if (tl.launch == null || c.lane == null) return tl;
    tl.join = tl.launch + ASCENT;
    tl.thJ = G.pads[c.pad == null ? 0 : c.pad].th + G.sE * G.joinLead;
    const wE = G.wE(c.lane);
    const window = (t0) => t0 + mod(G.sE * (G.thD - (tl.thJ + G.sE * wE * (t0 - tl.join))), TAU) / wE;
    if (tl.fail != null) {
      tl.home = window(Math.max(tl.fail + 1.0, tl.join + 0.5));
      tl.behind = tl.home + G.coast;
      tl.back = tl.behind + G.swing(c.lane);
      tl.reenter = tl.back + G.coast;
      tl.splash = tl.reenter + REENTRY;
      return tl;
    }
    if (c.completedAt == null) return tl;
    tl.depart = window(Math.max(s(c.completedAt) + 1.2, tl.join + 0.5));
    tl.arrive = tl.depart + G.coast;
    if (c.mergedAt == null || c.site == null) return tl;
    const wM = G.wM(c.lane), t1 = Math.max(s(c.mergedAt) + 2.0, tl.arrive + 2.0); // room to turn engine-first
    tl.deorbit = t1 + mod(G.sM * (G.sites[c.site].th - G.sM * G.leadDeo - (G.thA + G.sM * wM * (t1 - tl.arrive))), TAU) / wM;
    tl.touch = tl.deorbit + DESCENT;
    return tl;
  }

  /* Where the craft is at t. Returns null when there is nothing to draw.
   *   x, y, z      position (the base on the ground, the middle in flight)
   *   dx, dy, dz   unit nose direction
   *   kind         stack | csmlm | lm | ascent | cm   (which model)
   *   stage        stack only: 0 whole, 1 first stage gone, 2 only the third stage and spacecraft
   *   anchor       0 = position is the base, 1 = position is the middle
   *   mid          stack only: model-space height of the point at the position
   *   size         scale on top of SCALE
   *   stack        0..1 how much of the rocket is assembled
   *   burn         0..1 engine
   *   grow         0..1 pop-in / fade-out scale
   *   chute        0..1 parachutes
   *   mate         optional { x, y, dx, dy, a }: the command module left in lunar orbit
   */
  function poseAt(G, c, tl, t) {
    if (tl.created == null || t < tl.created) return null;
    const E = G.E, M = G.M;
    const o = { x: 0, y: 0, z: 0, dx: 0, dy: 1, dz: 0, kind: 'stack', stage: 0, anchor: 0, mid: 0, size: SIZE.flight, stack: 1, burn: 0, grow: 1, roll: 0, chute: 0, mate: null, phase: '', dead: false };
    const flying = tl.launch != null && c.lane != null && t >= tl.launch;
    const orbitE = (tt) => {
      const r = G.rE(c.lane), th = tl.thJ + G.sE * G.wE(c.lane) * (tt - tl.join), d = tan(th, G.sE);
      o.x = E.x + Math.cos(th) * r; o.y = E.y + Math.sin(th) * r;
      o.dx = d[0]; o.dy = d[1];
      o.anchor = 1; o.stage = 2; o.mid = MID.upper;
      o.roll = (tt - tl.join) * 0.35;
    };
    const along = (c4, s, flip) => {
      const p = bez(c4, s), d = bezTan(c4, Math.min(Math.max(s, 0.0001), 0.9999));
      const cf = Math.cos(flip || 0), sf = Math.sin(flip || 0);
      o.x = p[0]; o.y = p[1];
      o.dx = d[0] * cf - d[1] * sf; o.dy = d[0] * sf + d[1] * cf;
      o.anchor = 1;
    };

    if (!flying) {
      if (c.pad == null || tl.padAt == null || t < tl.padAt) return null; // no pad free yet
      const pad = G.pads[c.pad], g = G.surface(E, pad.th, pad.z, 0.05);
      o.x = g.pos[0]; o.y = g.pos[1]; o.z = g.pos[2];
      o.dx = g.n[0]; o.dy = g.n[1]; o.dz = g.n[2];
      o.grow = smooth((t - tl.padAt) / 0.6);
      o.size = SIZE.pad;
      // The stack rises while the plan is drafted and is topped off when it is parked.
      const drafted = (tt) => 0.74 * (1 - Math.exp(-Math.max(0, tt - tl.padAt) / 7));
      o.stack = tl.assembled == null || t < tl.assembled ? drafted(t) : lerp(drafted(tl.assembled), 1, smooth((t - tl.assembled) / ASSEMBLE));
      o.phase = 'pad';
      const end = tl.fail != null ? tl.fail : tl.abandon;
      if (end != null && t >= end) {
        o.dead = true;
        o.grow *= 1 - smooth((t - end - 0.3) / SCRUB_FADE);
        if (o.grow <= 0.001) return null;
      }
      return o;
    }

    if (t < tl.join) {
      const u = (t - tl.launch) / ASCENT, s = u * u;
      const pi = c.pad == null ? 0 : c.pad, a = G.ascent(pi, c.lane);
      along(a.c, s);
      o.z = G.pads[pi].z * (1 - smooth(u * 2.5));
      o.anchor = smooth(u * 3);
      o.mid = MID.stack * o.anchor + (MID.upper - MID.stack) * smooth((u - 0.3) / 0.45);
      o.stage = u < 0.4 ? 0 : u < 0.7 ? 1 : 2;
      o.size = lerp(SIZE.pad, SIZE.flight, smooth((u - 0.4) / 0.5));
      o.burn = smooth(u * 14) * (1 - 0.75 * smooth((u - 0.82) / 0.18));
      o.phase = 'ascent';
    } else if (tl.home != null && t >= tl.home) {
      // Free return: the same crossing out, round the far side without braking, the other crossing back.
      o.kind = 'csmlm';
      o.phase = 'free-return';
      if (t < tl.behind) {
        along(G.transfer(c.lane), (t - tl.home) / G.coast);
        if (t < tl.home + SHED) { o.kind = 'stack'; o.stage = 2; o.mid = MID.upper; }
        o.burn = 1 - smooth((t - tl.home) / 2.2);
        o.roll = (t - tl.home) * 0.5;
      } else if (t < tl.back) {
        const th = G.thA + G.sM * G.sweep * ((t - tl.behind) / (tl.back - tl.behind)), d = tan(th, G.sM), r = G.rM(c.lane);
        o.x = M.x + Math.cos(th) * r; o.y = M.y + Math.sin(th) * r;
        o.dx = d[0]; o.dy = d[1];
        o.anchor = 1;
        o.roll = (t - tl.home) * 0.5;
      } else if (t < tl.reenter) {
        const u = (t - tl.back) / G.coast;
        along(G.homeLeg(c.lane), u);
        if (u > 0.88) {
          // Only the command module comes back, blunt end first.
          o.kind = 'cm';
          o.size = SIZE.cm;
          o.dx = -o.dx; o.dy = -o.dy;
        } else o.roll = (t - tl.home) * 0.5;
      } else if (t < tl.splash) {
        // Through the atmosphere and down under the parachutes.
        const u = (t - tl.reenter) / REENTRY, p = G.reentry(c.lane, u), q = G.reentry(c.lane, Math.min(1, u + 0.02));
        const th = Math.atan2(p[1] - E.y, p[0] - E.x), hang = smooth((u - 0.4) / 0.45);
        let vx = q[0] - p[0], vy = q[1] - p[1];
        const vl = Math.hypot(vx, vy) || 1;
        vx = lerp(-vx / vl, Math.cos(th), hang); vy = lerp(-vy / vl, Math.sin(th), hang);
        const l = Math.hypot(vx, vy) || 1;
        o.kind = 'cm';
        o.size = SIZE.cm;
        o.x = p[0]; o.y = p[1];
        o.dx = vx / l; o.dy = vy / l;
        o.chute = smooth((u - 0.35) / 0.15);
        o.anchor = 1 - smooth((u - 0.8) / 0.2);
      } else {
        const g = G.surface(E, G.thS, 0, 0.02), dt = t - tl.splash;
        o.kind = 'cm';
        o.size = SIZE.cm;
        o.x = g.pos[0]; o.y = g.pos[1]; o.z = g.pos[2];
        o.dx = g.n[0]; o.dy = g.n[1]; o.dz = g.n[2];
        o.chute = 1 - smooth(dt / 1.2);
        o.grow = 1 - smooth((dt - SPLASH_HOLD) / SPLASH_FADE);
        if (o.grow <= 0.001) return null;
        o.phase = 'splash';
      }
      o.dead = true;
      return o;
    } else if (tl.depart == null || t < tl.depart) {
      orbitE(t);
      const burnAt = tl.depart != null ? tl.depart : tl.home;
      o.burn = Math.max(0.25 * (1 - smooth((t - tl.join) / 0.7)), burnAt == null ? 0 : smooth((t - (burnAt - 1.1)) / 1.1));
      o.phase = 'earth-orbit';
    } else if (t < tl.arrive) {
      // Turn engine-first for the insertion burn at the end of the coast.
      along(G.transfer(c.lane), (t - tl.depart) / G.coast, smooth((t - (tl.arrive - 2.8)) / 1.2) * Math.PI);
      if (t < tl.depart + SHED) { o.stage = 2; o.mid = MID.upper; }
      else o.kind = 'csmlm';
      o.burn = Math.max(1 - smooth((t - tl.depart) / 2.2), smooth((t - (tl.arrive - 1.3)) / 0.6));
      o.roll = (t - tl.depart) * 0.5;
      o.phase = 'coast';
    } else if (tl.deorbit == null || t < tl.deorbit) {
      const r = G.rM(c.lane), th = G.thA + G.sM * G.wM(c.lane) * (t - tl.arrive), d = tan(th, G.sM);
      const flip = (1 - smooth((t - tl.arrive - 0.5) / 1.3)) * Math.PI, cf = Math.cos(flip), sf = Math.sin(flip);
      o.kind = 'csmlm';
      o.x = M.x + Math.cos(th) * r; o.y = M.y + Math.sin(th) * r;
      o.dx = d[0] * cf - d[1] * sf; o.dy = d[0] * sf + d[1] * cf;
      o.anchor = 1;
      o.burn = 1 - smooth((t - tl.arrive) / 0.9);
      o.roll = (t - tl.arrive) * 0.3;
      o.phase = 'lunar-orbit';
    } else {
      const site = G.sites[c.site], g = G.surface(M, site.th, site.z, 0.05);
      if (t < tl.touch) {
        const u = (t - tl.deorbit) / DESCENT, s = 1 - (1 - u) * (1 - u);
        const c4 = G.descent(c.lane, c.site), p = bez(c4, s), d = bezTan(c4, Math.min(s, 0.9999));
        const settle = smooth((u - 0.5) / 0.5), k = settle * settle;
        o.kind = 'lm';
        o.size = lerp(SIZE.flight, SIZE.lm, smooth(u / 0.3));
        o.x = lerp(p[0], g.pos[0], k); o.y = lerp(p[1], g.pos[1], k); o.z = g.pos[2] * settle;
        // Engine first, swinging upright over the site.
        const flip = smooth((u - 0.04) / 0.2) * Math.PI, cf = Math.cos(flip), sf = Math.sin(flip), up = smooth((u - 0.45) / 0.4);
        const vx = lerp(d[0] * cf - d[1] * sf, g.n[0], up), vy = lerp(d[0] * sf + d[1] * cf, g.n[1], up), vz = g.n[2] * up;
        const l = Math.hypot(vx, vy, vz) || 1;
        o.dx = vx / l; o.dy = vy / l; o.dz = vz / l;
        o.anchor = 1 - smooth((u - 0.6) / 0.4);
        o.burn = smooth((u - 0.2) / 0.12) * (1 - smooth((u - 0.97) / 0.03));
        // The command module stays in orbit and drops out of the story.
        const r = G.rM(c.lane), th = G.thA + G.sM * G.wM(c.lane) * (t - tl.arrive), dm = tan(th, G.sM);
        o.mate = { x: M.x + Math.cos(th) * r, y: M.y + Math.sin(th) * r, dx: dm[0], dy: dm[1], a: 1 - smooth((t - tl.deorbit - 2.5) / 5) };
        o.phase = 'descent';
      } else {
        const dt = t - tl.touch;
        if (dt > STAY + LIFT) return null;
        const u = clamp01((dt - STAY) / LIFT), h = 5.5 * u * u;
        o.kind = 'ascent';
        o.size = SIZE.lm;
        o.x = g.pos[0] + g.n[0] * h; o.y = g.pos[1] + g.n[1] * h; o.z = g.pos[2] + g.n[2] * h;
        o.dx = g.n[0]; o.dy = g.n[1]; o.dz = g.n[2];
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

  // Height of what is drawn, in world units.
  function extent(p) {
    let h;
    if (p.kind === 'stack') h = p.stage === 2 ? HEIGHT.upper : p.stage === 1 ? 3.8 : HEIGHT.stack * Math.max(0.15, p.stack);
    else if (p.kind === 'csmlm') h = HEIGHT.csmlm;
    else if (p.kind === 'cm') h = HEIGHT.cm + 1.2 * p.chute;
    else h = p.kind === 'ascent' ? 0.5 : HEIGHT.lm;
    return h * SCALE * p.size * p.grow;
  }

  const MOVING = { ascent: 1, 'earth-orbit': 1, coast: 1, 'lunar-orbit': 1, descent: 1, 'free-return': 1 };
  /* Positions every dt seconds from t, nearest first: behind the craft for
   * dt > 0 (its trail), ahead of it for dt < 0 (the plotted course, which
   * stops where the craft arrives). */
  function trail(G, c, tl, t, n, dt) {
    const out = [];
    let first = null;
    for (let i = 0; i < n; i++) {
      const p = poseAt(G, c, tl, t - i * dt);
      if (!p || !MOVING[p.phase]) break;
      if (first == null) first = p.phase;
      if (dt < 0 && p.phase !== first) {
        out.push(p.x, p.y, p.z);
        break;
      }
      out.push(p.x, p.y, p.z);
    }
    return out;
  }

  const clock = (sec) => {
    sec = Math.max(0, Math.floor(sec));
    const h = Math.floor(sec / 3600), m = Math.floor((sec % 3600) / 60), s = sec % 60;
    return (h ? h + ':' + String(m).padStart(2, '0') : String(m)) + ':' + String(s).padStart(2, '0');
  };

  /* Where a craft is and what it is doing, in plain words. `place` is one of
   * pad, earth-orbit, to-moon, lunar-orbit, surface, home. */
  function describe(c, tl, pose, t) {
    const stage = KB.stageOf(c);
    if (c.status === 'failed') {
      if (tl.splash == null) return { place: 'pad', text: 'Failed on the pad' };
      return t >= tl.splash ? { place: 'home', text: 'Failed. Splashed down, crew safe' } : { place: 'home', text: 'Failed. Coming home on a free return' };
    }
    if (c.status === 'abandoned') return { place: tl.launch == null ? 'pad' : 'earth-orbit', text: 'Scrubbed' };
    if (stage === 'drafting') return { place: 'pad', text: c.assembledAt != null ? 'On the pad. Plan sent back for reshaping' : 'On the pad. Plan being drafted' };
    if (stage === 'reviewable') return { place: 'pad', text: 'On the pad. Plan ready for your go' };
    if (stage === 'queued') return { place: 'pad', text: 'On the pad. Go given, waiting for a free worker' };
    if (pose && pose.phase === 'ascent') return { place: 'earth-orbit', text: 'Lift-off' };
    if (stage === 'running') {
      const p = KB.progressOf(c), n = p.total ? ' (' + p.done + ' of ' + p.total + ' features)' : '';
      if (c.pendingGrant) return { place: 'earth-orbit', text: 'Earth orbit. Holding for a grant' };
      if (c.status === 'blocked') return { place: 'earth-orbit', text: 'Earth orbit. Blocked' };
      if (c.status === 'paused') return { place: 'earth-orbit', text: 'Earth orbit. Paused' };
      if (c.status === 'validating') return { place: 'earth-orbit', text: 'Earth orbit. Final checks' };
      if (c.milestones.some((x) => x.status === 'validating')) return { place: 'earth-orbit', text: 'Earth orbit. Validators checking' + n };
      const fix = c.milestones.some((x) => x.features.some((f) => f.origin === 'fix' && f.status === 'active'));
      return { place: 'earth-orbit', text: 'Earth orbit. ' + (fix ? 'Fixing a finding' : 'Building') + n };
    }
    if (stage === 'delivered') {
      if (tl.depart != null && t < tl.depart) return { place: 'earth-orbit', text: 'Complete. Leaving for the Moon in ' + clock(tl.depart - t + 1) };
      if (tl.arrive != null && t < tl.arrive) return { place: 'to-moon', text: 'Complete. Coasting to the Moon' };
      if (c.mergeRequestedAt != null) return { place: 'lunar-orbit', text: 'Lunar orbit. Merge gates running' };
      return { place: 'lunar-orbit', text: 'Lunar orbit. Complete, waiting for your go to land' };
    }
    if (stage === 'landed') {
      if (tl.touch == null || t >= tl.touch) return { place: 'surface', text: 'Landed. Merged' };
      if (tl.arrive != null && t < tl.arrive) return { place: 'to-moon', text: 'Merged. Coasting to the Moon' };
      return t < tl.deorbit ? { place: 'lunar-orbit', text: 'Go for landing. Descent in ' + clock(tl.deorbit - t + 1) } : { place: 'lunar-orbit', text: 'Powered descent' };
    }
    return { place: 'pad', text: stage };
  }

  KB.makeGeom = makeGeom;
  KB.timeline = timeline;
  KB.poseAt = poseAt;
  KB.trail = trail;
  KB.extent = extent;
  KB.describe = describe;
  KB.clock = clock;
  KB.bez = bez;
  KB.PROFILE = { ASCENT, DESCENT, STAY, LIFT, SCALE, HEIGHT, MID, SIZE };
})((globalThis.KB = globalThis.KB || {}));
