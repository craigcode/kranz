// Headless checks for the pure parts: simulator, fold and mission profile.
import { createRequire } from 'node:module';
const require = createRequire(import.meta.url);
globalThis.KB = {};
for (const f of ['sim', 'fold', 'profile']) require('../src/' + f + '.js');
const KB = globalThis.KB;
const LOAD = Date.UTC(2026, 9, 8, 20, 53, 0);
const mode = process.argv[2] || 'state';

function fleetAt(sim, fold, G, T) {
  const S = fold.at(sim.events, T);
  return S.order.map((id) => {
    const c = S.missions.get(id), tl = KB.timeline(G, c), pose = KB.poseAt(G, c, tl, T / 1000);
    return { c, tl, pose, d: KB.describe(c, tl, pose, T / 1000) };
  });
}
const rel = (s) => (s == null ? '   -  ' : ((s - LOAD / 1000) >= 0 ? '+' : '') + (s - LOAD / 1000).toFixed(1));

if (mode === 'state') {
  const sim = KB.createSim({ loadAt: LOAD }); sim.tick(LOAD);
  const fold = KB.createFold(), G = KB.makeGeom('wide');
  const dt = Number(process.argv[3] || 0);
  if (dt) sim.tick(LOAD + dt * 1000);
  const T = LOAD + dt * 1000;
  const S = fold.at(sim.events, T);
  console.log('events', sim.events.length, 'span', ((LOAD - sim.events[0].t) / 1000).toFixed(0) + 's');
  for (const { c, tl, pose, d } of fleetAt(sim, fold, G, T)) {
    console.log(`${c.slug.padEnd(26)} ${KB.stageOf(c).padEnd(10)} ${(pose ? pose.phase : 'hidden').padEnd(11)} lane ${c.lane ?? '-'} pad ${c.pad ?? '-'} site ${c.site ?? '-'}  [${d.text}]  launch ${rel(tl.launch)} done ${rel(c.completedAt && c.completedAt / 1000)} depart ${rel(tl.depart)} arrive ${rel(tl.arrive)} merged ${rel(c.mergedAt && c.mergedAt / 1000)} deorbit ${rel(tl.deorbit)} touch ${rel(tl.touch)} splash ${rel(tl.splash)}  $${c.costUsd.toFixed(2)}`);
  }
  console.log('pending:', KB.pendingOf(S).map((p) => `${p.kind}@${S.missions.get(p.missionId).slug} ${((T - p.askedAt) / 1000).toFixed(1)}s`).join(' | '));
  console.log('decisions:', S.decisions.map((d) => `${d.kind}:${d.decision} ${(d.latencyMs / 1000).toFixed(1)}s ${KB.LATENCY_LABELS[KB.latencyBucket(d.latencyMs)]}`).join(' | '));
}

if (mode === 'soak') {
  // Drive the sim for a while with a scripted viewer, then check invariants.
  const seconds = Number(process.argv[3] || 900), policy = process.argv[4] || 'mixed';
  for (const layout of ['wide', 'tall']) {
    const sim = KB.createSim({ loadAt: LOAD }); sim.tick(LOAD);
    const fold = KB.createFold(), G = KB.makeGeom(layout);
    let rnd = 12345; const rand = () => ((rnd = (rnd * 1103515245 + 12345) >>> 0) / 4294967296);
    const due = new Map(); const prev = new Map(); let jumps = 0, maxStep = 0, frames = 0, maxCraft = 0, laneClash = 0;
    const problems = [];
    for (let ms = 0; ms <= seconds * 1000; ms += 50) {
      const T = LOAD + ms; sim.tick(T);
      const S = fold.at(sim.events, T);
      if (ms % 1000 === 0 && policy !== 'idle') {
        for (const p of KB.pendingOf(S)) {
          if (!due.has(p.id + p.askedAt)) due.set(p.id + p.askedAt, T + (policy === 'fast' ? 2000 : 3000 + rand() * 25000));
          if (T >= due.get(p.id + p.askedAt)) {
            let choice = 'go';
            if (policy === 'mixed' && p.kind !== 'merge' && rand() < 0.25) choice = 'nogo';
            if (p.kind === 'question') choice = rand() < 0.5 ? 0 : 1;
            sim.act({ missionId: p.missionId, kind: p.kind, questionId: p.questionId, choice }, T);
          }
        }
      }
      const seen = new Map(); let n = 0;
      for (const id of S.order) {
        const c = S.missions.get(id), tl = KB.timeline(G, c), pose = KB.poseAt(G, c, tl, T / 1000);
        if (!pose) { prev.delete(id); continue; }
        n++;
        for (const k of ['x', 'y', 'z', 'dx', 'dy', 'dz', 'burn', 'grow', 'stack', 'anchor', 'chute']) if (!Number.isFinite(pose[k])) problems.push(`NaN ${k} ${c.slug} ${pose.phase} @${ms}`);
        const q = prev.get(id);
        if (q) {
          const d = Math.hypot(pose.x - q.x, pose.y - q.y, pose.z - q.z);
          if (d > maxStep) maxStep = d;
          if (d > 0.45) { jumps++; if (problems.length < 12) problems.push(`jump ${d.toFixed(2)} ${c.slug} ${q.phase}->${pose.phase} @${(ms / 1000).toFixed(2)}s`); }
          const turn = Math.acos(Math.max(-1, Math.min(1, pose.dx * q.dx + pose.dy * q.dy + pose.dz * q.dz)));
          if (turn > 0.5 && !pose.dead && problems.length < 12) problems.push(`snap ${(turn * 57.3).toFixed(0)}deg ${c.slug} ${q.phase}->${pose.phase} @${(ms / 1000).toFixed(2)}s`);
        }
        prev.set(id, pose);
        if (pose.phase === 'earth-orbit' || pose.phase === 'lunar-orbit' || pose.phase === 'coast') {
          const key = pose.phase + c.lane;
          if (seen.has(key)) laneClash++;
          seen.set(key, pose);
        }
      }
      maxCraft = Math.max(maxCraft, n); frames++;
      // Replay must agree with live: a fresh fold at T equals the running one.
      if (ms % 60000 === 0 && ms > 0) {
        fold.at(sim.events, T); // pick up anything the scripted viewer just decided
        const f2 = KB.createFold(), S2 = f2.at(sim.events, T);
        for (const id of S.order) {
          const a = S.missions.get(id), b = S2.missions.get(id);
          for (const k of ['status', 'lane', 'pad', 'padAt', 'site', 'startedAt', 'completedAt', 'mergedAt', 'costUsd']) if (a[k] !== b[k]) problems.push(`refold mismatch ${a.slug}.${k} ${a[k]} vs ${b[k]}`);
        }
      }
    }
    // Log shape: per-mission seq runs 1..n with no gaps; time never goes back.
    const seqs = new Map(); let lastT = -Infinity;
    for (const e of sim.events) {
      if (e.t < lastT) problems.push('time went backwards at ' + e.type);
      lastT = e.t;
      if (e.seq == null) { if (!e.type.startsWith('board.')) problems.push('missing seq on ' + e.type); continue; }
      const want = (seqs.get(e.missionId) || 0) + 1;
      if (e.seq !== want) problems.push(`seq gap ${e.missionId} ${e.seq} != ${want}`);
      seqs.set(e.missionId, e.seq);
    }
    const S = fold.at(sim.events, LOAD + seconds * 1000);
    const stages = {}; for (const id of S.order) { const st = KB.stageOf(S.missions.get(id)); stages[st] = (stages[st] || 0) + 1; }
    console.log(`[${layout}/${policy}] ${seconds}s: events ${sim.events.length}, missions ${S.order.length}, stages ${JSON.stringify(stages)}, max craft ${maxCraft}, max step ${maxStep.toFixed(3)}, jumps ${jumps}, lane clashes ${laneClash}, decisions ${S.decisions.length}`);
    for (const p of problems.slice(0, 14)) console.log('   !', p);
    if (problems.length || jumps || laneClash) process.exitCode = 1;
  }
}

if (mode === 'compose') {
  // The first frame should show one craft at every stage of the pipeline.
  const want = { 'board-kit-assets': 'landed', 'board-fleet-fold': 'landed', 'board-mission-profile': 'landed', 'board-scene': 'delivered', 'board-live-feed': 'delivered', 'board-holds-and-polls': 'running', 'board-replay': 'running', 'board-landed-observation': 'reviewable', 'board-slack-link': 'drafting' };
  const sim = KB.createSim({ loadAt: LOAD }); sim.tick(LOAD);
  let bad = 0;
  for (const layout of ['wide', 'tall']) {
    const fold = KB.createFold(), G = KB.makeGeom(layout), fleet = fleetAt(sim, fold, G, LOAD);
    const got = Object.fromEntries(fleet.map((f) => [f.c.slug, KB.stageOf(f.c)]));
    for (const k of Object.keys(want)) if (got[k] !== want[k]) { bad++; console.log(`! ${layout} ${k}: ${got[k]} (want ${want[k]})`); }
    const S = fold.at(sim.events, LOAD), pending = KB.pendingOf(S).map((p) => p.kind).sort().join(',');
    if (pending !== 'grant,merge,merge,plan') { bad++; console.log('! pending ' + pending); }
    const phases = fleet.map((f) => (f.pose ? f.pose.phase : 'hidden'));
    console.log(layout, phases.join(' '), '|', fleet.map((f) => f.d.text).join(' / '));
    // Nothing scripted for the backstory may still be due after load.
    const late = sim.events.filter((e) => e.t > LOAD).length; if (late) { bad++; console.log('! events after load: ' + late); }
  }
  console.log(bad ? 'COMPOSE FAILED' : 'compose ok');
  process.exitCode = bad ? 1 : 0;
}
