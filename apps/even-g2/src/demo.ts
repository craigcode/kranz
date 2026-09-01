import type { KranzApi, MissionState, MissionSummary } from './contracts'

const summaries: MissionSummary[] = [
  {
    id: 'm-a12f0c',
    repoId: 'kranz',
    repoDisplayName: 'Kranz',
    status: 'blocked',
    goal: 'Add signed release provenance and verify every artifact',
    createdAt: '2026-08-27T18:00:00Z',
  },
  {
    id: 'm-7bc231',
    repoId: 'kranz',
    repoDisplayName: 'Kranz',
    status: 'executing',
    goal: 'Repair the Windows fixture and restore green main',
    createdAt: '2026-08-27T17:00:00Z',
  },
  {
    id: 'm-92aa10',
    repoId: 'kranz',
    repoDisplayName: 'Kranz',
    status: 'complete',
    goal: 'Wire the Gas City queue into Kranz native dispatch',
    createdAt: '2026-08-27T16:00:00Z',
  },
]

const states = new Map<string, MissionState>([
  [
    'm-a12f0c',
    {
      mission: { status: 'blocked', goal: summaries[0]?.goal ?? '' },
      pendingGrantRequest: {
        milestoneId: 'ms-2',
        kind: 'egress',
        command: 'api.github.com:443',
      },
      pendingQuestions: [
        {
          questionId: 'q-17',
          role: 'worker',
          text: 'Which release-note tone should I use?',
          options: ['Direct and technical', 'Narrative and celebratory'],
        },
      ],
    },
  ],
  [
    'm-7bc231',
    { mission: { status: 'executing', goal: summaries[1]?.goal ?? '' } },
  ],
  [
    'm-92aa10',
    { mission: { status: 'complete', goal: summaries[2]?.goal ?? '' } },
  ],
])

export class DemoKranzApi implements KranzApi {
  listMissions(): Promise<MissionSummary[]> {
    return Promise.resolve(structuredClone(summaries))
  }

  missionState(id: string): Promise<MissionState> {
    const state = states.get(id)
    if (!state) return Promise.reject(new Error(`unknown demo mission ${id}`))
    return Promise.resolve(structuredClone(state))
  }

  approveGrant(id: string, command: string): Promise<void> {
    const state = states.get(id)
    if (state?.pendingGrantRequest?.command === command) delete state.pendingGrantRequest
    return Promise.resolve()
  }

  denyGrant(id: string, command: string, _reason: string): Promise<void> {
    return this.approveGrant(id, command)
  }

  answerQuestion(id: string, questionId: string, _answer: string, _option: number): Promise<void> {
    const state = states.get(id)
    if (state?.pendingQuestions) {
      state.pendingQuestions = state.pendingQuestions.filter(
        (question) => question.questionId !== questionId,
      )
    }
    return Promise.resolve()
  }
}
