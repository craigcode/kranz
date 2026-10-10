/* kranz.js: the same feed from a real `kranz serve`, read-only.
 *
 * Reads docs/protocol.md's REST surface with plain GETs:
 *   /api/health, /api/repos, /api/missions, /api/tickets,
 *   /api/missions/:id/events?since=<seq>, /api/missions/:id/pending-plan
 * (repo-scoped under /api/repos/:repoId when the host has a catalog) and
 * turns the three facts Kranz does not write to events.jsonl into `board.*`
 * observations, stamped when this page first saw them:
 *   the ticket a mission came from, a plan parked for review, a merged branch.
 *
 * It never POSTs. Answering a poll stays in Mission Control until the board
 * lives inside the dashboard and can use its token flow.
 */
(function (KB) {
  'use strict';

  const ACTIVE = { planning: 1, approved: 1, running: 1, paused: 1, blocked: 1, validating: 1 };
  const RECENT_MS = 36 * 3600 * 1000; // finished and merged missions older than this are left out
  const POLL_MS = 3000;

  function createKranzSource(opts) {
    const base = String(opts.base || '').replace(/\/+$/, '');
    const where = base ? base.replace(/^https?:\/\//, '') : location.host;
    const events = [];
    const missions = new Map();
    let prefixes = null, first = true, stopped = false;

    const src = {
      label: 'Kranz at ' + where,
      simulated: false,
      readOnly: true,
      events,
      rev: 0, // bumped when the log had to be re-sorted; the fold starts over
      status: { state: 'connecting', detail: '' },
      windowMs: 6 * 3600 * 1000,
      tick() {},
      act() { return false; },
      link(m) {
        const info = missions.get(m.id);
        return base + '/#' + (info && info.repo ? '/r/' + encodeURIComponent(info.repo) : '') + '/m/' + encodeURIComponent(m.id);
      },
      stop() { stopped = true; },
    };

    async function get(path) {
      const res = await fetch(base + path, { headers: { accept: 'application/json' } });
      if (!res.ok) {
        const err = new Error(path + ' answered ' + res.status);
        err.status = res.status;
        throw err;
      }
      // A stale serve answers unknown routes with the dashboard's HTML.
      if ((res.headers.get('content-type') || '').indexOf('json') < 0) throw new Error(path + ' did not answer with JSON');
      return res.json();
    }

    function ingest(batch) {
      if (!batch.length) return;
      batch.sort((a, b) => a.t - b.t);
      const tail = events.length ? events[events.length - 1].t : -Infinity;
      const inOrder = batch[0].t >= tail;
      for (const e of batch) events.push(e);
      if (!inOrder) {
        events.sort((a, b) => a.t - b.t);
        src.rev++;
      }
    }

    async function discover() {
      const health = await get('/api/health');
      if (health && health.version) src.label = 'Kranz ' + health.version + ' at ' + where;
      try {
        const repos = await get('/api/repos');
        const ok = Array.isArray(repos) ? repos.filter((r) => r.status !== 'unavailable') : [];
        if (ok.length) return ok.map((r) => ({ repo: r.id, path: '/api/repos/' + encodeURIComponent(r.id) }));
      } catch (err) {
        // No catalog: a single-repository serve.
      }
      return [{ repo: null, path: '/api' }];
    }

    async function cycle() {
      const now = Date.now(), batch = [];
      const observe = (m, type, payload, t) => batch.push({ seq: null, ts: new Date(t).toISOString(), t, missionId: m.id, type, payload });
      try {
        if (!prefixes) prefixes = await discover();
        for (const pf of prefixes) {
          const rows = await get(pf.path + '/missions');
          // An older serve may not have a route; ask once, then leave it alone.
          const tickets = pf.noTickets ? [] : await get(pf.path + '/tickets').catch(() => ((pf.noTickets = true), []));
          const ticketFor = new Map();
          for (const tk of Array.isArray(tickets) ? tickets : []) if (tk.missionId) ticketFor.set(tk.missionId, tk);

          for (const row of Array.isArray(rows) ? rows : []) {
            let m = missions.get(row.id);
            const active = !!ACTIVE[row.status];
            if (!m) {
              const stale = !active && row.merged === true && now - Date.parse(row.createdAt) > RECENT_MS;
              if (stale || row.status === 'deleted') continue;
              m = { id: row.id, repo: pf.repo, path: pf.path + '/missions/' + encodeURIComponent(row.id), lastSeq: 0, lastT: null, completedT: null, status: null, loaded: false, ticketed: false, parked: false, merged: false, seenUnmerged: false };
              missions.set(row.id, m);
            }

            if (!m.loaded || active || row.status !== m.status) {
              const list = await get(m.path + '/events?since=' + m.lastSeq);
              for (const e of Array.isArray(list) ? list : []) {
                if (e.seq > m.lastSeq) m.lastSeq = e.seq;
                e.t = Date.parse(e.ts);
                m.lastT = e.t;
                if (e.type === 'mission.completed') m.completedT = e.t;
                // Stream chatter of finished missions is not worth keeping.
                if (!active && e.type === 'worker.message') continue;
                batch.push(e);
              }
              m.loaded = true;
            }
            m.status = row.status;

            const tk = ticketFor.get(row.id);
            if (tk && !m.ticketed) {
              m.ticketed = true;
              observe(m, 'board.ticket', { slug: tk.slug, title: tk.title, priority: tk.priority }, first && m.lastT != null ? m.lastT : now);
            }

            if (row.status === 'planning') {
              let parked = tk ? tk.state === 'review' || tk.state === 'parked' : false, shape = null;
              if (!tk && !m.noPending) {
                const pp = await get(m.path + '/pending-plan').catch(() => ((m.noPending = true), null));
                parked = !!(pp && pp.pending);
                if (parked && pp.plan && pp.plan.milestones) shape = { milestones: pp.plan.milestones.length, features: pp.plan.milestones.reduce((n, ms) => n + (ms.features || []).length, 0) };
              }
              if (parked && !m.parked) observe(m, 'board.plan-parked', { estimate: null, milestones: shape ? shape.milestones : 0, features: shape ? shape.features : 0 }, first && m.lastT != null ? m.lastT : now);
              if (!parked && m.parked) observe(m, 'board.plan-reshaping', {}, now);
              m.parked = parked;
            }

            if (row.merged === false) m.seenUnmerged = true;
            if (row.merged === true && !m.merged) {
              m.merged = true;
              // Seen unmerged earlier in this session: it landed just now.
              // Otherwise it landed before we were looking; the closest known time is completion.
              observe(m, 'board.merged', {}, m.seenUnmerged ? now : (m.completedT != null ? m.completedT : m.lastT != null ? m.lastT : now) + 1);
            }
          }
        }
        ingest(batch);
        first = false;
        src.status = { state: 'live', detail: '' };
      } catch (err) {
        ingest(batch);
        src.status = { state: 'lost', detail: String((err && err.message) || err) };
      }
      if (!stopped) setTimeout(cycle, POLL_MS);
    }

    cycle();
    return src;
  }

  KB.createKranzSource = createKranzSource;
})((globalThis.KB = globalThis.KB || {}));
