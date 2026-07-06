// Pure, DOM-free counting helper for the missions header. Kept separate from
// MissionPicker so it can be unit-tested without rendering.

const FINISHED: ReadonlySet<string> = new Set(['complete', 'failed', 'abandoned']);
const RUNNING: ReadonlySet<string> = new Set(['approved', 'running', 'paused', 'blocked', 'validating']);

/** Any other status (planning, deleted, unknown) counts toward neither bucket. */
export function missionCounts(missions: { status: string }[]): { finished: number; running: number } {
  let finished = 0;
  let running = 0;
  for (const m of missions) {
    if (FINISHED.has(m.status)) finished += 1;
    else if (RUNNING.has(m.status)) running += 1;
  }
  return { finished, running };
}
