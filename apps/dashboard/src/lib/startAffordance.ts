import type { MissionState } from './types';

export function isApprovedIdle(state: MissionState): boolean {
  return state.mission.status === 'approved';
}
