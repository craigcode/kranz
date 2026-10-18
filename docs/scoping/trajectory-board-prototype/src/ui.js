/* ui.js: the room around the board. Owns the clock (live or replay), turns
 * the folded fleet into labels, the "your move" list, the flight log and the
 * timeline, and hands the operator's decisions to the source.
 */
(function (KB) {
  'use strict';

  const $ = (id) => document.getElementById(id);
  const esc = (s) => String(s).replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[c]);
  const clamp01 = (v) => (v < 0 ? 0 : v > 1 ? 1 : v);
  const utc = (ms) => new Date(ms).toISOString().slice(11, 19);
  const money = (v) => '$' + v.toFixed(2);
  const plural = (n, w) => n + ' ' + w + (n === 1 ? '' : 's');
  // What each kind of ask is called, on its card and on the craft's label.
  const KIND = {
    plan: { kicker: 'Launch', chip: 'GO FOR LAUNCH?' },
    grant: { kicker: 'Grant', chip: 'ALLOW?' },
    block: { kicker: 'Blocked', chip: 'BLOCKED' },
    merge: { kicker: 'Landing', chip: 'GO FOR LANDING?' },
    question: { kicker: 'Question', chip: 'QUESTION' },
  };
  const URGENT = { grant: 0, block: 1, plan: 2, merge: 3, question: 4 };

  /* The simulated feed, unless the page is opened on this machine with
   * #kranz: then it reads a running `kranz serve` instead (see kranz.js).
   * #kranz@http://127.0.0.1:4570 names a server on another port. Only a
   * loopback address is accepted; anything else falls back to the default. */
  const LOOPBACK = /^http:\/\/(?:localhost|127\.0\.0\.1|\[::1\]):\d{1,5}$/;
  function pickSource(loadAt) {
    const local = /^(localhost|127\.0\.0\.1|\[::1\])$/.test(location.hostname);
    const m = /^#kranz(?:@(.+))?$/.exec(location.hash);
    if (local && m) {
      const dev = location.port === '5173' || location.port === '1420';
      const named = m[1] && LOOPBACK.test(m[1]) ? m[1] : '';
      return KB.createKranzSource({ base: named || (dev ? 'http://127.0.0.1:4560' : '') });
    }
    const sim = KB.createSim({ loadAt });
    sim.tick(loadAt);
    return sim;
  }

  function start() {
    const loadAt = Date.now();
    const source = pickSource(loadAt);
    const fold = KB.createFold();

    const board = $('board'), chips = $('chips'), pollsEl = $('polls'), tapeEl = $('tape');
    const scrub = $('scrub'), marks = $('marks'), cardEl = $('card'), hailEl = $('hail'), legendEl = $('legend'), noteEl = $('note'), feedEl = $('feed');
    const chart = KB.createChart($('chart'));
    const scene = KB.createScene($('gl'));
    const still = window.matchMedia('(prefers-reduced-motion: reduce)').matches;

    let G = null, layout = null, view = { w: 0, h: 0, k: 1 }, cardOverlay = true;
    let mode = 'live', playing = true, speed = 4, T = loadAt, dragging = false;
    let selected = null, lastFrame = performance.now(), lastUi = 0, lastVersion = -1, lastTapeIdx = -1, lastMode = '', lastRev = source.rev || 0;
    let S = fold.at(source.events, T);
    const chipEls = new Map(), pollEls = new Map(), seenPending = new Set();
    let hailTimer = 0, colors = {};

    // --- colours for the canvas layers come from the page's tokens ----------
    function readColors() {
      const cs = getComputedStyle(document.documentElement), v = (n) => cs.getPropertyValue(n).trim();
      colors = {
        ink: v('--ink'), dim: v('--ink-dim'), line: v('--ink-line'), line2: v('--ink-line2'), hold: v('--hold'),
        trackAlpha: Number(v('--track')) || 0.05, outlineAlpha: Number(v('--track-edge')) || 0.2,
        burn: v('--burn'), earthGlow: v('--earth-glow'), moonGlow: v('--moon-glow'), flag: v('--flag'),
        markHold: v('--red'), markLanded: v('--accent'),
      };
      chart.setColors(colors);
    }
    readColors();
    window.matchMedia('(prefers-color-scheme: dark)').addEventListener('change', readColors);
    new MutationObserver(readColors).observe(document.documentElement, { attributes: true, attributeFilter: ['data-theme'] });

    // --- framing -------------------------------------------------------------
    function fit() {
      const w = board.clientWidth || 800, h = board.clientHeight || 500;
      const want = w / h < 0.95 ? 'tall' : 'wide';
      if (want !== layout) {
        layout = want;
        G = KB.makeGeom(layout);
        if (scene) scene.setGeom(G);
      }
      const b = G.bounds, k = Math.min((w - 20) / (b.x1 - b.x0), (h - 20) / (b.y1 - b.y0));
      const dpr = Math.min(window.devicePixelRatio || 1, 2);
      view = { w, h, dpr, k, cx: (b.x0 + b.x1) / 2, cy: (b.y0 + b.y1) / 2 };
      chart.setView(w, h, dpr, k, view.cx, view.cy);
      if (scene) scene.setView(w, h, dpr, k, view.cx, view.cy);
      board.classList.toggle('compact', w < 620);
      cardOverlay = getComputedStyle(cardEl).position === 'absolute';
    }

    // --- chips: one label per craft, kept out of each other's way -----------
    function chipFor(id) {
      let el = chipEls.get(id);
      if (!el) {
        el = document.createElement('button');
        el.type = 'button';
        el.className = 'chip';
        el.innerHTML = '<b></b><i></i>';
        el.dataset.id = id;
        chips.appendChild(el);
        chipEls.set(id, el);
        el._text = null;
      }
      return el;
    }
    function overlap(a, b) {
      const w = Math.min(a.x + a.w, b.x + b.w) - Math.max(a.x, b.x), h = Math.min(a.y + a.h, b.y + b.h) - Math.max(a.y, b.y);
      return w > 0 && h > 0 ? w * h : 0;
    }
    function candidates(it) {
      const w = it.w, h = it.h, out = [];
      if (it.ground) {
        // Up the local vertical from the craft, fanned out in levels.
        const nx = it.nx, ny = it.ny, tipX = it.px + nx * (it.rad + 6), tipY = it.py + ny * (it.rad + 6);
        for (let lvl = 0; lvl < 6; lvl++) {
          const d = lvl * (h + 3) + h / 2 + 2, x = tipX + nx * d - w / 2, y = tipY + ny * d - h / 2;
          out.push({ x, y, w, h });
          if (lvl < 3) out.push({ x: x + w / 2 + 8, y, w, h }, { x: x - w / 2 - 8, y, w, h });
        }
      } else {
        const d = it.rad + 9, e = d * 0.72;
        out.push({ x: it.px + d, y: it.py - h / 2, w, h }, { x: it.px - d - w, y: it.py - h / 2, w, h });
        out.push({ x: it.px + e, y: it.py - e - h, w, h }, { x: it.px - e - w, y: it.py - e - h, w, h });
        out.push({ x: it.px + e, y: it.py + e, w, h }, { x: it.px - e - w, y: it.py + e, w, h });
        out.push({ x: it.px - w / 2, y: it.py - d - h, w, h }, { x: it.px - w / 2, y: it.py + d, w, h });
      }
      return out;
    }
    function placeChips(list, obstacles) {
      const placed = obstacles.slice();
      const order = list.slice().sort((a, b) => (a.rank - b.rank) || (a.px - b.px));
      for (const it of order) {
        const cand = candidates(it);
        const score = (r) => {
          let s = 0;
          for (const p of placed) s += overlap(r, p);
          for (const o of list) if (o !== it) s += overlap(r, { x: o.px - o.rad, y: o.py - o.rad, w: o.rad * 2, h: o.rad * 2 }) * 0.7;
          if (r.x < 4) s += (4 - r.x) * 60;
          if (r.y < 4) s += (4 - r.y) * 60;
          if (r.x + r.w > view.w - 4) s += (r.x + r.w - view.w + 4) * 60;
          if (r.y + r.h > view.h - 4) s += (r.y + r.h - view.h + 4) * 60;
          return s;
        };
        // First clear spot wins; a chip only gives up a clear spot it already
        // holds for one that is clearly nearer, so labels do not flicker.
        let best = 0, bestScore = Infinity;
        for (let i = 0; i < cand.length; i++) {
          const s = score(cand[i]);
          if (s < bestScore - 0.5) {
            best = i;
            bestScore = s;
          }
          if (s === 0) break;
        }
        const keep = it.el._slot;
        if (keep != null && keep < cand.length && keep !== best && keep - best < 3 && score(cand[keep]) <= bestScore) best = keep;
        it.el._slot = best;
        it.chip = cand[best];
        placed.push(it.chip);
      }
    }

    // --- polls: "your move" ---------------------------------------------------
    function pollBody(p, c) {
      if (p.kind === 'plan') {
        const e = c.estimate, shape = c.planShape;
        return {
          title: 'Go for launch?',
          body: 'Plan parked' + (shape && shape.features ? ': ' + plural(shape.milestones, 'milestone') + ', ' + plural(shape.features, 'feature') : '') + '.' + (e ? ' Estimate ' + money(e.expectedUsd) + ' (' + money(e.lowUsd) + ' to ' + money(e.highUsd) + ').' : ''),
          go: ['GO', 'Approves the plan and queues the mission'], nogo: ['NO-GO', 'Sends the plan back to be reshaped'],
        };
      }
      if (p.kind === 'grant') {
        const g = c.pendingGrant, left = source.grantTimeoutSec ? source.grantTimeoutSec - (T - g.askedAt) / 1000 : 0;
        return {
          title: g.kind === 'egress' ? 'Allow this destination?' : g.kind === 'touch-path' ? 'Allow writes to this path?' : 'Allow this command?',
          code: g.command,
          body: 'Work on ' + g.milestoneId + ' is stopped until you answer.' + (left > 0 ? ' Refuses itself in ' + KB.clock(left) + '.' : ''),
          go: ['GO', 'Grants it for this mission'], nogo: ['NO-GO', 'Denies it; the milestone blocks'],
        };
      }
      if (p.kind === 'block') {
        return {
          title: 'Unblock ' + (c.blockedMilestone || 'the milestone') + '?',
          body: 'Blocked: ' + (c.blockReason || 'no reason recorded') + '.',
          go: ['UNBLOCK', 'Sends guidance and resumes'], nogo: ['ABANDON', 'Retires the mission'],
        };
      }
      if (p.kind === 'merge') {
        const r = c.mergeRefused;
        return {
          title: 'Go for landing?',
          body: (r ? 'Last attempt refused (' + r.status + '): ' + r.gate + '. ' : '') + 'Merges ' + (c.missionBranch || 'the mission branch') + ' into ' + (c.baseBranch || 'main') + ' once the merge gates pass.',
          go: ['GO', 'Runs the merge gates, then merges'],
        };
      }
      const q = c.questions.find((x) => x.questionId === p.questionId) || { text: '', options: [] };
      return { title: q.text, body: 'Work carries on meanwhile. Your answer reaches the worker as guidance.', options: q.options };
    }
    function renderPolls() {
      const pending = KB.pendingOf(S), live = mode === 'live', seen = new Set(), note = $('poll-note');
      $('poll-count').textContent = pending.length;
      $('poll-empty').hidden = pending.length > 0;
      note.hidden = pending.length === 0 || (live && !source.readOnly);
      note.textContent = !live ? 'You are looking at the past, so these are read-only. Go live to answer.' : 'This board only reads from the server. Answer in Mission Control and the craft will move.';
      let prev = null;
      for (const p of pending) {
        const c = S.missions.get(p.missionId), b = pollBody(p, c), k = KIND[p.kind];
        seen.add(p.id);
        let el = pollEls.get(p.id);
        const sig = b.title + '|' + (b.code || '') + '|' + (b.options || []).join('|');
        if (!el || el._sig !== sig) {
          if (el) el.remove();
          el = document.createElement('article');
          el.className = 'poll';
          el._sig = sig;
          let acts;
          if (source.readOnly) acts = '<a class="btn" href="' + esc(source.link(c)) + '" target="_blank" rel="noopener">Open in Mission Control</a>';
          else if (b.options) acts = b.options.map((o, i) => '<button type="button" class="btn" data-choice="' + i + '">' + esc(o) + '</button>').join('');
          else acts = '<button type="button" class="btn go" data-choice="go" title="' + esc(b.go[1]) + '">' + b.go[0] + '</button>' + (b.nogo ? '<button type="button" class="btn nogo" data-choice="nogo" title="' + esc(b.nogo[1]) + '">' + b.nogo[0] + '</button>' : '');
          el.innerHTML =
            '<header><span class="kicker">' + k.kicker + '</span><span class="clock" title="Time since the ask"></span></header>' +
            '<h3>' + esc(b.title) + '</h3>' +
            '<button type="button" class="who" data-select="' + esc(p.missionId) + '">' + esc(c.slug) + '</button>' +
            (b.code ? '<code>' + esc(b.code) + '</code>' : '') +
            '<p class="body"></p>' +
            '<footer><span class="acts" data-mission="' + esc(p.missionId) + '" data-kind="' + p.kind + '"' + (p.questionId ? ' data-q="' + esc(p.questionId) + '"' : '') + '>' + acts + '</span></footer>';
          pollEls.set(p.id, el);
        }
        if (prev ? prev.nextSibling !== el : pollsEl.firstChild !== el) pollsEl.insertBefore(el, prev ? prev.nextSibling : pollsEl.firstChild);
        prev = el;
        el.querySelector('.clock').textContent = KB.clock((T - p.askedAt) / 1000);
        el.querySelector('.body').textContent = b.body;
        el.querySelectorAll('.acts button').forEach((n) => (n.disabled = !live));
        el.classList.toggle('sel', selected === p.missionId);
      }
      for (const [id, el] of pollEls) if (!seen.has(id)) {
        el.remove();
        pollEls.delete(id);
      }
      // Hail anything new that arrived while watching live.
      for (const p of pending) {
        const key = p.id + '@' + p.askedAt;
        if (seenPending.has(key)) continue;
        seenPending.add(key);
        if (live && p.askedAt > loadAt) hail(p, S.missions.get(p.missionId));
      }
    }
    pollsEl.addEventListener('click', (ev) => {
      const sel = ev.target.closest('[data-select]');
      if (sel) return select(sel.dataset.select);
      const btn = ev.target.closest('.acts button');
      if (!btn || mode !== 'live') return;
      const a = btn.parentNode.dataset, choice = a.kind === 'question' ? Number(btn.dataset.choice) : btn.dataset.choice;
      source.act({ missionId: a.mission, kind: a.kind, questionId: a.q, choice });
      lastVersion = -1;
    });

    /* The hail: a strip across the top of the board when something new needs
     * you. It is a picture of the event and sends nothing anywhere. Reaching a
     * phone is the job of Kranz's Slack bridge (`kranz serve --slack`), which
     * posts the same asks as cards with buttons. */
    function hail(p, c) {
      const k = KIND[p.kind], trim = (v) => String(v || '').trim().replace(/[.\s]+$/, '');
      let text;
      if (p.kind === 'plan') text = ' is on the pad with a plan to review. Go or no-go for launch?';
      else if (p.kind === 'grant') {
        const g = c.pendingGrant || {};
        text = ' is holding in orbit. It wants to ' + (g.kind === 'egress' ? 'reach ' : g.kind === 'touch-path' ? 'write to ' : 'run ') + trim(g.command) + '. Go or no-go?';
      } else if (p.kind === 'block') text = ' is blocked: ' + (trim(c.blockReason) || 'no reason recorded') + '. Unblock or abandon?';
      else if (p.kind === 'merge') text = ' is complete and on its way to the Moon. Go for landing?';
      else text = ' asks: ' + ((c.questions.find((x) => x.questionId === p.questionId) || {}).text || 'a question');
      hailEl.innerHTML = '<span>' + k.kicker + '</span><p><b>' + esc(c.slug) + '</b>' + esc(text) + '</p><button type="button" data-select="' + esc(p.missionId) + '">Show</button>';
      hailEl.hidden = false;
      hailEl.classList.remove('in');
      void hailEl.offsetWidth;
      hailEl.classList.add('in');
      clearTimeout(hailTimer);
      hailTimer = setTimeout(() => (hailEl.hidden = true), 9000);
    }
    hailEl.addEventListener('click', (ev) => {
      const sel = ev.target.closest('[data-select]');
      if (sel && selected !== sel.dataset.select) select(sel.dataset.select);
      hailEl.hidden = true;
    });

    // --- flight log: one line per turn of the story, not per event ---------
    function line(e, c, lat) {
      const p = e.payload || {}, by = lat && lat.latencyMs != null ? ' (you, after ' + Math.max(1, Math.round(lat.latencyMs / 1000)) + ' s)' : ' (you)';
      switch (e.type) {
        case 'board.plan-parked': return ['is on the pad with a plan ready', 'wait'];
        case 'board.plan-reshaping': return ['no-go, plan sent back' + by];
        case 'plan.approved': return ['go for launch' + by];
        case 'milestone.started': return p.milestoneId === 'ms-1' ? ['lifted off'] : null;
        case 'validation.finding': return ['validator found a ' + p.finding.severity + ' problem; fixing it'];
        case 'grant.requested': return ['is holding: wants ' + p.command, 'wait'];
        case 'grant.approved': return ['grant allowed' + by];
        case 'grant.denied': return String(p.reason || '').indexOf('timed out') === 0 ? ['grant timed out and was refused'] : ['grant refused' + by];
        case 'milestone.blocked': return ['is blocked: ' + p.reason, 'wait'];
        case 'milestone.unblocked': return ['unblocked' + by];
        case 'question.opened': return ['asks: ' + p.text, 'wait'];
        case 'question.answered': return ['answered: ' + p.answer + by];
        case 'mission.completed': return ['is complete and leaving for the Moon'];
        case 'board.merge-requested': return ['go for landing' + by];
        case 'board.merge-refused': return ['landing waved off: ' + p.gate + ' failed', 'wait'];
        case 'board.merged': return ['merged into ' + (c.baseBranch || 'main') + '. The Eagle has landed', 'landed'];
        case 'mission.failed': return ['failed: ' + String(p.reason || 'no reason given').replace(/[.\s]+$/, '') + '. Coming home', 'dead'];
        case 'mission.abandoned': return ['scrubbed: ' + String(p.reason || 'no reason given').replace(/[.\s]+$/, ''), 'dead'];
        default: return null;
      }
    }
    function renderTape() {
      const lat = new Map();
      for (const d of S.decisions) lat.set(d.missionId + '@' + d.at, d);
      const rows = [];
      for (let i = S.idx - 1; i >= 0 && rows.length < 40; i--) {
        const e = source.events[i], c = S.missions.get(e.missionId);
        const l = line(e, c, lat.get(e.missionId + '@' + e.t));
        if (!l) continue;
        rows.push('<li' + (l[1] ? ' data-tone="' + l[1] + '"' : '') + '><time>' + utc(e.t).slice(0, 5) + '</time><span class="msg"><b>' + esc(c.slug) + '</b> ' + esc(l[0]) + '</span></li>');
      }
      tapeEl.innerHTML = rows.join('');
      $('tape-empty').hidden = rows.length > 0;
    }

    // --- craft card ----------------------------------------------------------
    function select(id) {
      selected = selected === id ? null : id;
      lastVersion = -1;
    }
    function renderCard() {
      const c = selected && S.missions.get(selected);
      if (!c || c.createdAt == null) {
        cardEl.hidden = true;
        return;
      }
      const tl = KB.timeline(G, c), pose = KB.poseAt(G, c, tl, T / 1000), d = KB.describe(c, tl, pose, T / 1000);
      const runs = Object.keys(c.runs).length, msg = c.lastMessage, waiting = KB.pendingOf(S).some((p) => p.missionId === c.id);
      const pip = (f) => '<i class="pip ' + f.status + (f.origin === 'fix' ? ' fix' : '') + '" title="' + esc(f.id + ' ' + f.title + ' (' + f.status + ')') + '"></i>';
      const ms = c.milestones.map((m) => '<li><span class="t">' + esc(m.title) + '</span><span class="pips">' + m.features.map(pip).join('') + '</span></li>').join('');
      const html =
        '<header><b>' + esc(c.slug) + '</b><button type="button" id="card-close" aria-label="Close">×</button></header>' +
        '<p class="title">' + esc(c.title) + '</p>' +
        '<p class="state' + (waiting ? ' wait' : '') + '">' + esc(d.text) + '</p>' +
        (ms ? '<ol class="ms">' + ms + '</ol>' : '') +
        (c.status === 'failed' || c.status === 'abandoned' ? '<p class="quiet">' + esc(c.failReason || '') + '</p>' : msg && KB.stageOf(c) === 'running' ? '<p class="quiet">' + esc(msg.content) + '</p>' : '') +
        '<p class="meta">' + esc(c.id) + (runs ? ' · ' + plural(runs, 'run') + ' · ' + money(c.costUsd) : '') + '</p>';
      if (cardEl._html !== html) {
        cardEl._html = html;
        cardEl.innerHTML = html;
      }
      cardEl.hidden = false;
    }
    cardEl.addEventListener('click', (ev) => {
      if (ev.target.id === 'card-close') select(selected);
    });
    chips.addEventListener('click', (ev) => {
      const el = ev.target.closest('.chip');
      if (el) select(el.dataset.id);
    });
    // Tap a craft on the board to open its card.
    board.addEventListener('click', (ev) => {
      if (ev.target.closest('.chip, .hail, .legend')) return;
      const r = board.getBoundingClientRect(), x = ev.clientX - r.left, y = ev.clientY - r.top;
      let best = null, bd = 26;
      for (const [id, el] of chipEls) {
        if (!el._at) continue;
        const d = Math.hypot(el._at[0] - x, el._at[1] - y);
        if (d < bd) {
          bd = d;
          best = id;
        }
      }
      if (best) select(best);
    });

    // --- timeline --------------------------------------------------------------
    function setMode(m, play) {
      mode = m;
      playing = play;
      lastVersion = -1;
    }
    scrub.addEventListener('pointerdown', () => (dragging = true));
    scrub.addEventListener('input', () => {
      T = Number(scrub.value);
      setMode(T >= Number(scrub.max) - 300 ? 'live' : 'replay', false);
    });
    const release = () => {
      if (!dragging) return;
      dragging = false;
      if (mode === 'replay') playing = true;
    };
    scrub.addEventListener('pointerup', release);
    scrub.addEventListener('pointercancel', release);
    $('live').addEventListener('click', () => setMode('live', true));
    $('play').addEventListener('click', () => {
      if (mode === 'live') setMode('replay', false);
      else playing = !playing;
      lastVersion = -1;
    });
    document.querySelectorAll('.speed button').forEach((b) =>
      b.addEventListener('click', () => {
        speed = Number(b.dataset.speed);
        lastVersion = -1;
      })
    );

    function renderTime(now) {
      const first = source.events.length ? source.events[0].t : loadAt;
      const t0 = source.windowMs ? Math.max(first, now - source.windowMs) : first;
      scrub.min = t0;
      scrub.max = now;
      if (!dragging) scrub.value = T;
      const live = mode === 'live';
      $('when').textContent = live ? utc(T) + 'Z' : utc(T) + 'Z · ' + KB.clock((now - T) / 1000) + ' behind';
      $('live').classList.toggle('on', live);
      $('live').setAttribute('aria-pressed', live);
      const play = $('play'), paused = !live && !playing;
      play.dataset.state = paused ? 'paused' : 'playing';
      play.setAttribute('aria-label', paused ? 'Play' : 'Pause');
      document.querySelectorAll('.speed button').forEach((b) => b.setAttribute('aria-pressed', Number(b.dataset.speed) === speed));
      $('speed').classList.toggle('idle', live);
      scrub.style.setProperty('--fill', (clamp01((T - t0) / Math.max(1, now - t0)) * 100).toFixed(2) + '%');

      // Marks along the track: red where something began waiting on you, a flag for each landing.
      const w = marks.clientWidth, h = marks.clientHeight, dpr = Math.min(window.devicePixelRatio || 1, 2);
      if (!w) return;
      if (marks.width !== Math.round(w * dpr)) {
        marks.width = Math.round(w * dpr);
        marks.height = Math.round(h * dpr);
      }
      const ctx = marks.getContext('2d'), X = (t) => 7 + ((t - t0) / Math.max(1, now - t0)) * (w - 14);
      ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
      ctx.clearRect(0, 0, w, h);
      for (const e of source.events) {
        if (e.t < t0) continue;
        const x = X(e.t);
        if (e.type === 'board.plan-parked' || e.type === 'grant.requested' || e.type === 'milestone.blocked' || e.type === 'mission.completed' || e.type === 'question.opened') {
          ctx.fillStyle = colors.markHold;
          ctx.fillRect(x - 0.75, h - 7, 1.5, 7);
        } else if (e.type === 'board.merged') {
          ctx.fillStyle = colors.markLanded;
          ctx.fillRect(x - 0.5, 1, 1, h - 1);
          ctx.fillRect(x, 1, 5, 4);
        }
      }
    }

    function renderStats() {
      let flight = 0, landed = 0;
      for (const id of S.order) {
        const c = S.missions.get(id), st = KB.stageOf(c);
        if (st === 'landed') {
          // Merged is Kranz's "landed"; on the board it counts once the craft is down.
          const tl = KB.timeline(G, c);
          if (tl.touch == null || T / 1000 >= tl.touch) landed++;
          else flight++;
        } else if (c.startedAt != null && (st === 'running' || st === 'delivered')) flight++;
      }
      const waiting = KB.pendingOf(S).length;
      $('st-wait').textContent = waiting;
      $('st-wait').parentNode.classList.toggle('alert', waiting > 0);
      $('st-flight').textContent = flight;
      $('st-landed').textContent = landed;
      $('summary').textContent = 'Trajectory board at ' + utc(T) + ' UTC. ' + (waiting ? plural(waiting, 'craft') + ' waiting on you' : 'Nothing waiting on you') + ', ' + flight + ' in flight, ' + landed + ' landed.';

      // Where the feed comes from, and anything the viewer should know about it.
      const st = source.status;
      feedEl.textContent = source.label + (source.readOnly ? ' · read-only' : '');
      feedEl.dataset.state = source.simulated ? 'sim' : st.state;
      let note = '';
      if (st && st.state === 'lost') note = 'Cannot reach the server: ' + st.detail + '. Retrying.';
      else if (st && st.state === 'connecting') note = 'Connecting to the server.';
      else if (!source.simulated && S.order.length === 0) note = 'The server has no recent missions to show.';
      else if (!scene) note = 'The 3D models could not load here, so craft are shown as markers. Everything else works.';
      noteEl.textContent = note;
      noteEl.hidden = !note;
    }

    // --- frame -----------------------------------------------------------------
    function frame(nowPerf) {
      requestAnimationFrame(frame);
      const now = Date.now(), dt = Math.min(0.25, (nowPerf - lastFrame) / 1000);
      lastFrame = nowPerf;
      source.tick(now);
      if (mode === 'live') T = now;
      else if (playing && !dragging) {
        T += dt * 1000 * speed;
        if (T >= now) {
          T = now;
          mode = 'live';
        }
      }
      if ((source.rev || 0) !== lastRev) {
        // The source re-ordered its log (late events from another mission).
        lastRev = source.rev;
        fold.reset();
      }
      S = fold.at(source.events, T);
      const t = T / 1000, wall = nowPerf / 1000;

      // The most pressing thing each mission is waiting on you for, if anything.
      const asks = new Map();
      for (const p of KB.pendingOf(S)) {
        const cur = asks.get(p.missionId);
        if (!cur || URGENT[p.kind] < URGENT[cur.kind]) asks.set(p.missionId, p);
      }
      const crafts = [], tls = new Map();
      for (const id of S.order) {
        const c = S.missions.get(id), tl = KB.timeline(G, c);
        tls.set(id, tl);
        const pose = KB.poseAt(G, c, tl, t);
        if (!pose) continue;
        const H = KB.extent(pose), lift = pose.kind === 'stack' ? clamp01(pose.mid / KB.PROFILE.MID.stack) : pose.anchor, off = (H / 2) * (1 - lift);
        const px = chart.toPx(G, pose.x + pose.dx * off, pose.y + pose.dy * off, pose.z + pose.dz * off);
        const ph = pose.phase, ground = ph === 'pad' || ph === 'landed' || ph === 'splash';
        const between = ph === 'ascent' || ph === 'coast' || ph === 'descent' || ph === 'free-return';
        const up = G.project(pose.dx, pose.dy, pose.dz), ask = pose.dead ? null : asks.get(id) || null;
        crafts.push({
          id, c, tl, pose, ask, wait: !!ask, px: px[0], py: px[1], rad: Math.max(9, H * view.k * 0.5), ground,
          nx: up[0], ny: -up[1], selected: selected === id,
          trail: ground || ph === 'liftoff' ? null : KB.trail(G, c, tl, t, 16, 0.13),
          ahead: between ? KB.trail(G, c, tl, t, 40, -0.45) : null,
        });
      }

      // Landing sites: the descent stage and flag each landing leaves behind.
      const NS = G.sites.length, sites = new Array(NS).fill(null), flags = [];
      let landedN = 0;
      for (let i = 0; i < S.landings.length; i++) {
        const tl = tls.get(S.landings[i]), k = i % NS;
        if (tl && tl.deorbit != null && t < tl.deorbit) continue; // not on its way down yet
        if (tl && tl.touch != null && t < tl.touch) {
          sites[k] = null; // coming down onto this site
          continue;
        }
        sites[k] = { stage: 1, flag: tl && tl.touch != null ? clamp01((t - tl.touch - 2.5) / 0.8) : 1 };
        landedN++;
      }
      for (let k = 0; k < NS; k++) if (sites[k]) {
        const g = G.surface(G.M, G.sites[k].th, G.sites[k].z, 0.05);
        flags.push(chart.toPx(G, g.pos[0], g.pos[1], g.pos[2]));
      }

      // Labels. A craft that is waiting on you says what for; the rest only give their name.
      const seen = new Set(), compact = view.w < 620, items = [];
      // On a small board, drop whatever every slug starts with ("board-").
      let cutAt = 0;
      if (compact && crafts.length > 1) {
        const a = crafts[0].c.slug;
        let n = a.length;
        for (const cr of crafts) {
          let i = 0;
          while (i < n && cr.c.slug[i] === a[i]) i++;
          n = i;
        }
        cutAt = a.lastIndexOf('-', n - 1) + 1;
      }
      for (const cr of crafts) {
        const ph = cr.pose.phase;
        if (ph === 'liftoff' && !cr.selected) continue; // the lander's label goes when its ascent stage leaves
        const el = chipFor(cr.id);
        const sub = cr.ask ? KIND[cr.ask.kind].chip : cr.c.status === 'failed' ? 'FAILED' : cr.c.status === 'abandoned' ? 'SCRUBBED' : ph === 'landed' ? 'LANDED' : '';
        const name = compact && cr.ask ? '' : cr.c.slug.slice(cutAt);
        const tone = cr.ask ? 'wait' : cr.pose.dead ? 'dead' : ph === 'landed' ? 'landed' : '';
        const text = name + '|' + sub + '|' + tone;
        seen.add(cr.id);
        if (el._text !== text) {
          el._text = text;
          el.firstChild.textContent = name;
          el.lastChild.textContent = sub;
          el.firstChild.hidden = !name;
          el.lastChild.hidden = !sub;
          el.dataset.tone = tone;
          el.setAttribute('aria-label', cr.c.slug + ': ' + KB.describe(cr.c, cr.tl, cr.pose, t).text);
          el._w = el.offsetWidth;
          el._h = el.offsetHeight;
        }
        el.setAttribute('aria-pressed', cr.selected);
        cr.el = el;
        cr.w = el._w;
        cr.h = el._h;
        cr.rank = cr.ask ? 0 : cr.selected ? 1 : 2;
        items.push(cr);
      }
      for (const [id, el] of chipEls) if (!seen.has(id)) {
        el.remove();
        chipEls.delete(id);
      }
      const obstacles = [];
      const box = (el) => obstacles.push({ x: el.offsetLeft, y: el.offsetTop, w: el.offsetWidth, h: el.offsetHeight });
      if (!cardEl.hidden && cardOverlay) box(cardEl);
      if (!hailEl.hidden) obstacles.push({ x: (view.w - hailEl.offsetWidth) / 2, y: hailEl.offsetTop, w: hailEl.offsetWidth, h: hailEl.offsetHeight });
      box(legendEl);
      for (const [P, a0, a1, top] of [[G.E, 52, 116, 3.0], [G.M, 24, 156, 1.3]]) {
        // Keep labels off the pads and the landing ground.
        const rad = Math.PI / 180, lo = P.R * Math.min(Math.sin(a0 * rad), Math.sin(a1 * rad));
        const a = chart.toPx(G, P.x + Math.cos(a1 * rad) * (P.R + top), P.y + lo, 0), b = chart.toPx(G, P.x + Math.cos(a0 * rad) * (P.R + top), P.y + P.R + top, 0);
        obstacles.push({ x: a[0], y: b[1], w: b[0] - a[0], h: a[1] - b[1] });
      }
      placeChips(items, obstacles);
      for (const cr of items) {
        cr.el.style.transform = 'translate(' + Math.round(cr.chip.x) + 'px,' + Math.round(cr.chip.y) + 'px)';
        cr.el._at = [cr.px, cr.py];
      }

      chart.draw({ G, crafts, flags, wall, still, noGL: !scene || !scene.ready });
      if (scene && scene.ready) scene.frame({ crafts, sites, landed: landedN, wall, still });

      if (S.version !== lastVersion || nowPerf - lastUi > 250 || mode !== lastMode) {
        const changed = S.version !== lastVersion || mode !== lastMode;
        lastUi = nowPerf;
        lastVersion = S.version;
        lastMode = mode;
        renderPolls();
        renderStats();
        renderTime(now);
        renderCard();
        if (changed || S.idx !== lastTapeIdx) {
          lastTapeIdx = S.idx;
          renderTape();
        }
      }
    }

    new ResizeObserver(fit).observe(board);
    fit();
    if (document.fonts && document.fonts.ready) document.fonts.ready.then(() => {
      for (const el of chipEls.values()) el._text = null;
    });
    // Holds that were already open when the page loaded do not get a hail.
    for (const p of KB.pendingOf(S)) seenPending.add(p.id + '@' + p.askedAt);

    if (scene) scene.init(G, globalThis.KIT || null).then(fit);
    requestAnimationFrame(frame);
    // For the console: KB.debug.source.events is the log the board is drawing.
    KB.debug = { source, fold, scene, chart, get G() { return G; }, get view() { return view; }, get T() { return T; }, get S() { return S; } };
  }

  KB.start = start;
})((globalThis.KB = globalThis.KB || {}));
