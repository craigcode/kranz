// Fleet model for the Trajectory Board lens — docs/scoping/trajectory-board.md (slice 2).
// Pure reduction from pipeline rows + live MissionState map into FlightCraft poses,
// places, urgency-ordered asks, counts, and single-step animated transitions.

import { pendingAsks, type InFlightAsk, type InFlightAskKind } from '../pendingAsks';
import { buildRows, type Row } from '../pipelineRows';
import { pipelineStage, type PipelineStage } from '../pipelineStage';
import { visible } from '../presentation';
import { missionHash, repoHash, repoHashFor, repoIdFromHash, ticketHash } from '../routes';
import type { MissionState, MissionSummary, TicketSummary } from '../types';
import type { FlightCraft } from './flightPlan';

export const PAD_CAPACITY = 4;
export const LANE_CAPACITY = 5;
export const SITE_CAPACITY = 12;
export const AFLOAT_CAPACITY = 3;

export type CraftPlace =
  | 'hangar'
  | 'pad'
  | 'earth-orbit'
  | 'lunar-orbit'
  | 'lunar-surface'
  | 'free-return'
  | 'abandoned';

export type StageAskKind = 'needs-you' | 'wrong-plan' | 'reviewable' | 'delivered' | 'failed';
export type BoardAskKind = InFlightAskKind | StageAskKind;

export const ASK_URGENCY: Readonly<Record<BoardAskKind, number>> = {
  permission: 0,
  grant: 1,
  blocked: 2,
  gate: 3,
  revision: 4,
  question: 5,
  'needs-you': 6,
  'wrong-plan': 7,
  reviewable: 8,
  delivered: 9,
  failed: 10,
};

export interface BoardAsk {
  id: string;
  craftKey: string;
  kind: BoardAskKind;
  kindLabel: string;
  chipPhrase: string;
  workItemId: string;
  title: string;
  detail: string;
  href: string;
  missionId?: string;
  slug?: string;
  createdAtMs: number | null;
  row: Row;
}

export interface FleetCraft {
  key: string;
  id: string;
  title: string;
  stage: PipelineStage;
  place: CraftPlace;
  ring: boolean;
  label: string;
  chipPhrase: string;
  angleSlot: number | null;
  missionId?: string;
  slug?: string;
  row: Row;
  craft: FlightCraft;
}

export interface StageTransition {
  key: string;
  id: string;
  from: PipelineStage;
  to: PipelineStage;
  at: number;
}

export interface HangarSummary {
  total: number;
  captured: number;
  overflow: number;
  ringed: number;
}

export interface FleetCounts {
  waiting: number;
  inFlight: number;
  landed: number;
}

export interface FleetModel {
  nowMs: number;
  crafts: FleetCraft[];
  byKey: ReadonlyMap<string, FleetCraft>;
  asks: BoardAsk[];
  counts: FleetCounts;
  hangar: HangarSummary;
  transitions: StageTransition[];
}

const GROUND_STAGES: ReadonlySet<PipelineStage> = new Set<PipelineStage>([
  'drafting',
  'needs-you',
  'wrong-plan',
  'reviewable',
  'queued',
]);

const RINGED_GROUND_STAGES: ReadonlySet<PipelineStage> = new Set<PipelineStage>([
  'needs-you',
  'wrong-plan',
  'reviewable',
]);

const ORBITAL_STAGES: ReadonlySet<PipelineStage> = new Set<PipelineStage>(['running', 'delivered']);

const KIND_LABELS: Readonly<Record<BoardAskKind, string>> = {
  permission: 'One-call permission',
  grant: 'Grant request',
  blocked: 'Blocked',
  gate: 'Gate review',
  revision: 'Plan revision',
  question: 'Question',
  'needs-you': 'needs-you',
  'wrong-plan': 'wrong-plan',
  reviewable: 'reviewable',
  delivered: 'delivered',
  failed: 'failed',
};

const CHIP_PHRASES: Readonly<Record<BoardAskKind, string>> = {
  permission: 'ALLOW ONCE?',
  grant: 'ALLOW?',
  blocked: 'BLOCKED',
  gate: 'GATE REVIEW',
  revision: 'REVISION?',
  question: 'QUESTION',
  'needs-you': 'NEEDS YOU',
  'wrong-plan': 'WRONG PLAN',
  reviewable: 'GO FOR LAUNCH?',
  delivered: 'GO FOR LANDING?',
  failed: 'FAILED',
};

function parseCreatedMs(iso: string | undefined): number | null {
  if (!iso) return null;
  const ms = Date.parse(iso);
  return Number.isFinite(ms) ? ms : null;
}

function scopedLinks(hash?: string) {
  if (hash === undefined) {
    return {
      mission: (id: string) => missionHash(id),
      ticket: (slug: string) => ticketHash(slug),
      actionable: () => repoHash(''),
    };
  }
  const repoId = repoIdFromHash(hash);
  const forSuffix = (suffix: string): string => {
    if (repoId !== null) return repoHashFor(repoId, suffix);
    return suffix === '' ? '#/' : `#/${suffix.replace(/^\//, '')}`;
  };
  return {
    mission: (id: string) => forSuffix(`m/${encodeURIComponent(id)}`),
    ticket: (slug: string) => forSuffix(`backlog/${encodeURIComponent(slug)}`),
    actionable: () => forSuffix(''),
  };
}

function isSingleStepTransition(from: PipelineStage, to: PipelineStage): boolean {
  if (from === to) return false;
  if (to === 'abandoned') return true;
  if (GROUND_STAGES.has(from) && to === 'running') return true;
  if (from === 'running' && to === 'delivered') return true;
  if (from === 'delivered' && to === 'landed') return true;
  if (from === 'running' && to === 'failed') return true;
  return false;
}

function steadyStateCraft(
  stage: PipelineStage,
  nowMs: number,
  pad: number | null,
  lane: number | null,
  angleSlot: number | null,
  site: number | null,
  afloat: boolean,
): FlightCraft {
  const slotOffsetMs = (angleSlot ?? 0) * 5_300 + (lane ?? 0) * 1_700;
  switch (stage) {
    case 'captured':
      return {
        createdAt: null,
        padAt: null,
        assembledAt: null,
        startedAt: null,
        completedAt: null,
        mergedAt: null,
        failedAt: null,
        abandonedAt: null,
        pad: null,
        lane: null,
        site: null,
        afloat: false,
      };
    case 'drafting':
      return {
        createdAt: nowMs - 20_000,
        padAt: pad !== null ? nowMs - 14_000 : null,
        assembledAt: null,
        startedAt: null,
        completedAt: null,
        mergedAt: null,
        failedAt: null,
        abandonedAt: null,
        pad,
        lane: null,
        site: null,
        afloat: false,
      };
    case 'needs-you':
    case 'wrong-plan':
    case 'reviewable':
    case 'queued':
      return {
        createdAt: nowMs - 30_000,
        padAt: pad !== null ? nowMs - 25_000 : null,
        assembledAt: nowMs - 10_000,
        startedAt: null,
        completedAt: null,
        mergedAt: null,
        failedAt: null,
        abandonedAt: null,
        pad,
        lane: null,
        site: null,
        afloat: false,
      };
    case 'running':
      return {
        createdAt: nowMs - 120_000 - slotOffsetMs,
        padAt: nowMs - 110_000 - slotOffsetMs,
        assembledAt: nowMs - 100_000 - slotOffsetMs,
        startedAt: nowMs - 60_000 - slotOffsetMs,
        completedAt: null,
        mergedAt: null,
        failedAt: null,
        abandonedAt: null,
        pad: 0,
        lane: lane ?? 0,
        site: null,
        afloat: false,
      };
    case 'delivered':
      return {
        createdAt: nowMs - 240_000 - slotOffsetMs,
        padAt: nowMs - 230_000 - slotOffsetMs,
        assembledAt: nowMs - 220_000 - slotOffsetMs,
        startedAt: nowMs - 180_000 - slotOffsetMs,
        completedAt: nowMs - 120_000 - slotOffsetMs,
        mergedAt: null,
        failedAt: null,
        abandonedAt: null,
        pad: 0,
        lane: lane ?? 0,
        site: null,
        afloat: false,
      };
    case 'landed':
      return {
        createdAt: nowMs - 400_000,
        padAt: nowMs - 390_000,
        assembledAt: nowMs - 380_000,
        startedAt: nowMs - 360_000,
        completedAt: nowMs - 330_000,
        mergedAt: nowMs - 260_000,
        failedAt: null,
        abandonedAt: null,
        pad: 0,
        lane: 0,
        site,
        afloat: false,
      };
    case 'failed':
      return {
        createdAt: nowMs - 360_000,
        padAt: nowMs - 350_000,
        assembledAt: nowMs - 340_000,
        startedAt: nowMs - 320_000,
        completedAt: null,
        mergedAt: null,
        failedAt: nowMs - 260_000,
        abandonedAt: null,
        pad: 0,
        lane: 0,
        site: null,
        afloat,
      };
    case 'abandoned':
      return {
        createdAt: nowMs - 300_000,
        padAt: nowMs - 290_000,
        assembledAt: nowMs - 280_000,
        startedAt: null,
        completedAt: null,
        mergedAt: null,
        failedAt: null,
        abandonedAt: nowMs - 240_000,
        pad: 0,
        lane: null,
        site: null,
        afloat: false,
      };
    default:
      return stage satisfies never;
  }
}

function runningProgressSuffix(state: MissionState | undefined): string {
  if (!state) return '';
  const features = state.mission.milestones.flatMap((m) => m.features);
  if (features.length === 0) return '';
  const done = features.filter((f) => f.status === 'complete').length;
  return ` (${done}/${features.length})`;
}

export function buildFleetModel(
  tickets: TicketSummary[],
  missions: MissionSummary[],
  missionStates: ReadonlyMap<string, MissionState>,
  nowMs: number,
  prev: FleetModel | null = null,
  hash?: string,
): FleetModel {
  const rows = buildRows(tickets, missions);
  const links = scopedLinks(hash);

  interface DraftEntry {
    rowIndex: number;
    row: Row;
    stage: PipelineStage;
    createdAtMs: number | null;
    inFlightAsks: InFlightAsk[];
    ring: boolean;
  }

  const entries: DraftEntry[] = rows.map((row, rowIndex) => {
    const stage = pipelineStage(row.item);
    const createdAtMs = parseCreatedMs(row.missionCreatedAt);
    const state = row.missionId !== undefined ? missionStates.get(row.missionId) : undefined;
    const inFlightAsks = stage === 'running' ? pendingAsks(state, nowMs) : [];
    const ring =
      stage === 'running'
        ? inFlightAsks.length > 0
        : stage === 'needs-you' ||
          stage === 'wrong-plan' ||
          stage === 'reviewable' ||
          stage === 'delivered' ||
          stage === 'failed';
    return { rowIndex, row, stage, createdAtMs, inFlightAsks, ring };
  });

  // 1. Allocate 4 ground pads (ringed first, then oldest first).
  const groundEntries = entries
    .filter((e) => GROUND_STAGES.has(e.stage))
    .sort((a, b) => {
      const aRing = RINGED_GROUND_STAGES.has(a.stage) ? 0 : 1;
      const bRing = RINGED_GROUND_STAGES.has(b.stage) ? 0 : 1;
      if (aRing !== bRing) return aRing - bRing;
      if (a.createdAtMs !== null && b.createdAtMs !== null && a.createdAtMs !== b.createdAtMs) {
        return a.createdAtMs - b.createdAtMs;
      }
      return a.rowIndex - b.rowIndex;
    });

  const padByKey = new Map<string, number>();
  const eligibleGround = groundEntries.slice(0, PAD_CAPACITY);
  const usedPads = new Set<number>();
  for (const e of eligibleGround) {
    const prevPad = prev?.byKey.get(e.row.key)?.craft.pad;
    if (prevPad != null && prevPad >= 0 && prevPad < PAD_CAPACITY && !usedPads.has(prevPad)) {
      padByKey.set(e.row.key, prevPad);
      usedPads.add(prevPad);
    }
  }
  let nextPad = 0;
  for (const e of eligibleGround) {
    if (padByKey.has(e.row.key)) continue;
    while (nextPad < PAD_CAPACITY && usedPads.has(nextPad)) {
      nextPad += 1;
    }
    if (nextPad < PAD_CAPACITY) {
      padByKey.set(e.row.key, nextPad);
      usedPads.add(nextPad);
      nextPad += 1;
    }
  }

  // 2. Allocate 5 orbit lanes + angle slots for active orbital craft.
  const orbitalEntries = entries.filter((e) => ORBITAL_STAGES.has(e.stage));
  const laneSlotByKey = new Map<string, { lane: number; angleSlot: number }>();
  const usedLaneSlots = new Set<string>();
  for (const e of orbitalEntries) {
    const prevEntry = prev?.byKey.get(e.row.key);
    if (
      prevEntry &&
      ORBITAL_STAGES.has(prevEntry.stage) &&
      prevEntry.craft.lane != null &&
      prevEntry.angleSlot != null
    ) {
      const slotKey = `${prevEntry.craft.lane}:${prevEntry.angleSlot}`;
      if (!usedLaneSlots.has(slotKey)) {
        laneSlotByKey.set(e.row.key, {
          lane: prevEntry.craft.lane,
          angleSlot: prevEntry.angleSlot,
        });
        usedLaneSlots.add(slotKey);
      }
    }
  }
  let nextSlot = 0;
  for (const e of orbitalEntries) {
    if (laneSlotByKey.has(e.row.key)) continue;
    for (;;) {
      const lane = nextSlot % LANE_CAPACITY;
      const angleSlot = Math.floor(nextSlot / LANE_CAPACITY);
      nextSlot += 1;
      const slotKey = `${lane}:${angleSlot}`;
      if (!usedLaneSlots.has(slotKey)) {
        laneSlotByKey.set(e.row.key, { lane, angleSlot });
        usedLaneSlots.add(slotKey);
        break;
      }
    }
  }

  // 3. Allocate 12 landing sites to the 12 most recently created landed items.
  const landedEntries = entries
    .filter((e) => e.stage === 'landed')
    .sort((a, b) => {
      const aMs = a.createdAtMs ?? -Infinity;
      const bMs = b.createdAtMs ?? -Infinity;
      if (aMs !== bMs) return bMs - aMs;
      return b.rowIndex - a.rowIndex;
    });
  const eligibleLanded = landedEntries.slice(0, SITE_CAPACITY);
  const siteByKey = new Map<string, number>();
  const usedSites = new Set<number>();
  for (const e of eligibleLanded) {
    const prevSite = prev?.byKey.get(e.row.key)?.craft.site;
    if (prevSite != null && prevSite >= 0 && prevSite < SITE_CAPACITY && !usedSites.has(prevSite)) {
      siteByKey.set(e.row.key, prevSite);
      usedSites.add(prevSite);
    }
  }
  let nextSite = 0;
  for (const e of eligibleLanded) {
    if (siteByKey.has(e.row.key)) continue;
    while (nextSite < SITE_CAPACITY && usedSites.has(nextSite)) {
      nextSite += 1;
    }
    if (nextSite < SITE_CAPACITY) {
      siteByKey.set(e.row.key, nextSite);
      usedSites.add(nextSite);
      nextSite += 1;
    }
  }

  // 4. Keep the 3 most recently created failed items afloat.
  const failedEntries = entries
    .filter((e) => e.stage === 'failed')
    .sort((a, b) => {
      const aMs = a.createdAtMs ?? -Infinity;
      const bMs = b.createdAtMs ?? -Infinity;
      if (aMs !== bMs) return bMs - aMs;
      return b.rowIndex - a.rowIndex;
    });
  const afloatKeys = new Set<string>(failedEntries.slice(0, AFLOAT_CAPACITY).map((e) => e.row.key));

  // 5. Build asks, crafts, and transitions.
  const asksWithOrder: Array<{ ask: BoardAsk; rowIndex: number }> = [];
  const crafts: FleetCraft[] = [];
  const byKey = new Map<string, FleetCraft>();
  const transitions: StageTransition[] = [];

  for (const e of entries) {
    const { row, stage, createdAtMs, inFlightAsks, ring, rowIndex } = e;
    const safeId = visible(row.id);
    const safeTitle = visible(row.title);
    const state = row.missionId !== undefined ? missionStates.get(row.missionId) : undefined;

    // Collect asks for this work item
    const itemAsks: BoardAsk[] = [];
    if (stage === 'running') {
      const href = row.missionId !== undefined ? links.mission(row.missionId) : links.actionable();
      for (const ask of inFlightAsks) {
        itemAsks.push({
          id: `${row.key}:${ask.id}`,
          craftKey: row.key,
          kind: ask.kind,
          kindLabel: KIND_LABELS[ask.kind],
          chipPhrase: CHIP_PHRASES[ask.kind],
          workItemId: safeId,
          title: safeTitle,
          detail: visible(ask.detail),
          href,
          missionId: row.missionId,
          slug: row.slug,
          createdAtMs,
          row,
        });
      }
    } else if (stage === 'needs-you' || stage === 'wrong-plan') {
      const href =
        row.slug !== undefined
          ? links.ticket(row.slug)
          : row.missionId !== undefined
            ? links.mission(row.missionId)
            : links.actionable();
      itemAsks.push({
        id: `${row.key}:${stage}`,
        craftKey: row.key,
        kind: stage,
        kindLabel: KIND_LABELS[stage],
        chipPhrase: CHIP_PHRASES[stage],
        workItemId: safeId,
        title: safeTitle,
        detail: stage === 'needs-you' ? 'Answer + redraft' : 'Edit + redraft',
        href,
        missionId: row.missionId,
        slug: row.slug,
        createdAtMs,
        row,
      });
    } else if (stage === 'reviewable') {
      const href =
        row.slug !== undefined
          ? links.actionable()
          : row.missionId !== undefined
            ? links.mission(row.missionId)
            : links.actionable();
      itemAsks.push({
        id: `${row.key}:reviewable`,
        craftKey: row.key,
        kind: 'reviewable',
        kindLabel: KIND_LABELS.reviewable,
        chipPhrase: CHIP_PHRASES.reviewable,
        workItemId: safeId,
        title: safeTitle,
        detail: row.slug !== undefined ? 'Plan ready — Queue for run' : 'Plan ready — Approve plan',
        href,
        missionId: row.missionId,
        slug: row.slug,
        createdAtMs,
        row,
      });
    } else if (stage === 'delivered') {
      const href =
        row.missionId !== undefined
          ? links.mission(row.missionId)
          : row.slug !== undefined
            ? links.ticket(row.slug)
            : links.actionable();
      itemAsks.push({
        id: `${row.key}:delivered`,
        craftKey: row.key,
        kind: 'delivered',
        kindLabel: KIND_LABELS.delivered,
        chipPhrase: CHIP_PHRASES.delivered,
        workItemId: safeId,
        title: safeTitle,
        detail: 'Complete, unmerged — Merge',
        href,
        missionId: row.missionId,
        slug: row.slug,
        createdAtMs,
        row,
      });
    } else if (stage === 'failed') {
      const href =
        row.slug !== undefined
          ? links.ticket(row.slug)
          : row.missionId !== undefined
            ? links.mission(row.missionId)
            : links.actionable();
      itemAsks.push({
        id: `${row.key}:failed`,
        craftKey: row.key,
        kind: 'failed',
        kindLabel: KIND_LABELS.failed,
        chipPhrase: CHIP_PHRASES.failed,
        workItemId: safeId,
        title: safeTitle,
        detail: 'Failed — Redraft',
        href,
        missionId: row.missionId,
        slug: row.slug,
        createdAtMs,
        row,
      });
    }

    for (const ask of itemAsks) {
      asksWithOrder.push({ ask, rowIndex });
    }

    // Place
    const pad = GROUND_STAGES.has(stage) ? (padByKey.get(row.key) ?? null) : null;
    const laneSlot = ORBITAL_STAGES.has(stage) ? (laneSlotByKey.get(row.key) ?? null) : null;
    const site = stage === 'landed' ? (siteByKey.get(row.key) ?? null) : null;
    const afloat = stage === 'failed' ? afloatKeys.has(row.key) : false;

    let place: CraftPlace;
    if (stage === 'captured') place = 'hangar';
    else if (GROUND_STAGES.has(stage)) place = pad !== null ? 'pad' : 'hangar';
    else if (stage === 'running') place = 'earth-orbit';
    else if (stage === 'delivered') place = 'lunar-orbit';
    else if (stage === 'landed') place = 'lunar-surface';
    else if (stage === 'failed') place = 'free-return';
    else place = 'abandoned';

    // FlightCraft timestamps: steady state on first sight, single-step transition when applicable
    const prevEntry = prev?.byKey.get(row.key);
    let craft: FlightCraft;

    if (prevEntry && prevEntry.stage === stage) {
      // Unchanged stage: preserve timestamps; update slot/capacity flags cleanly.
      const prevCraft = prevEntry.craft;
      if (GROUND_STAGES.has(stage)) {
        craft = {
          ...prevCraft,
          pad,
          padAt: pad !== null ? (prevCraft.padAt ?? nowMs - 25_000) : null,
        };
      } else if (ORBITAL_STAGES.has(stage)) {
        craft = {
          ...prevCraft,
          lane: laneSlot?.lane ?? prevCraft.lane ?? 0,
        };
      } else if (stage === 'landed') {
        craft = {
          ...prevCraft,
          site,
        };
      } else if (stage === 'failed') {
        craft = {
          ...prevCraft,
          afloat,
        };
      } else {
        craft = { ...prevCraft };
      }
    } else if (prevEntry && isSingleStepTransition(prevEntry.stage, stage)) {
      transitions.push({
        key: row.key,
        id: safeId,
        from: prevEntry.stage,
        to: stage,
        at: nowMs,
      });
      const prevCraft = prevEntry.craft;
      if (stage === 'running') {
        craft = {
          createdAt: prevCraft.createdAt ?? nowMs - 30_000,
          padAt: prevCraft.padAt ?? nowMs - 25_000,
          assembledAt: prevCraft.assembledAt ?? nowMs - 5_000,
          startedAt: nowMs,
          completedAt: null,
          mergedAt: null,
          failedAt: null,
          abandonedAt: null,
          pad: prevCraft.pad ?? 0,
          lane: laneSlot?.lane ?? 0,
          site: null,
          afloat: false,
        };
      } else if (stage === 'delivered') {
        craft = {
          ...prevCraft,
          lane: laneSlot?.lane ?? prevCraft.lane ?? 0,
          completedAt: nowMs,
          mergedAt: null,
          failedAt: null,
          abandonedAt: null,
        };
      } else if (stage === 'landed') {
        craft = {
          ...prevCraft,
          lane: prevCraft.lane ?? 0,
          mergedAt: nowMs,
          site,
          afloat: false,
        };
      } else if (stage === 'failed') {
        craft = {
          ...prevCraft,
          lane: prevCraft.lane ?? 0,
          failedAt: nowMs,
          afloat,
        };
      } else {
        // any stage -> abandoned
        craft = {
          ...prevCraft,
          abandonedAt: nowMs,
          afloat: false,
        };
      }
    } else {
      craft = steadyStateCraft(
        stage,
        nowMs,
        pad,
        laneSlot?.lane ?? null,
        laneSlot?.angleSlot ?? null,
        site,
        afloat,
      );
    }

    const topAsk = itemAsks.length > 0 ? itemAsks[0] : null;
    const chipPhrase = topAsk
      ? topAsk.chipPhrase
      : stage === 'landed'
        ? 'LANDED'
        : stage === 'abandoned'
          ? 'SCRUBBED'
          : '';
    const label =
      stage === 'running' ? `${safeId}${runningProgressSuffix(state)}` : safeId;

    const fleetCraft: FleetCraft = {
      key: row.key,
      id: safeId,
      title: safeTitle,
      stage,
      place,
      ring,
      label,
      chipPhrase,
      angleSlot: laneSlot?.angleSlot ?? null,
      missionId: row.missionId,
      slug: row.slug,
      row,
      craft,
    };
    crafts.push(fleetCraft);
    byKey.set(row.key, fleetCraft);
  }

  // Sort asks by urgency, then oldest mission first (createdAtMs asc), then rowIndex.
  asksWithOrder.sort((a, b) => {
    const uA = ASK_URGENCY[a.ask.kind];
    const uB = ASK_URGENCY[b.ask.kind];
    if (uA !== uB) return uA - uB;
    if (a.ask.createdAtMs !== null && b.ask.createdAtMs !== null && a.ask.createdAtMs !== b.ask.createdAtMs) {
      return a.ask.createdAtMs - b.ask.createdAtMs;
    }
    if (a.ask.createdAtMs !== null && b.ask.createdAtMs === null) return -1;
    if (a.ask.createdAtMs === null && b.ask.createdAtMs !== null) return 1;
    return a.rowIndex - b.rowIndex;
  });

  const asks = asksWithOrder.map((x) => x.ask);

  const capturedCount = entries.filter((e) => e.stage === 'captured').length;
  const overflowGround = groundEntries.slice(PAD_CAPACITY);
  const ringedOverflow = overflowGround.filter((e) => e.ring).length;

  const hangar: HangarSummary = {
    total: capturedCount + overflowGround.length,
    captured: capturedCount,
    overflow: overflowGround.length,
    ringed: ringedOverflow,
  };

  const counts: FleetCounts = {
    waiting: asks.length,
    inFlight: entries.filter((e) => e.stage === 'running' || e.stage === 'delivered').length,
    landed: entries.filter((e) => e.stage === 'landed').length,
  };

  return {
    nowMs,
    crafts,
    byKey,
    asks,
    counts,
    hangar,
    transitions,
  };
}
