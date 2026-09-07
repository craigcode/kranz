export interface MissionSummary {
  id: string
  status: string
  goal: string
  repoId?: string
  repoDisplayName?: string
  createdAt?: string
  error?: string
}

export interface RepoSummary {
  id: string
  displayName: string
  pinned: boolean
  isDefault: boolean
  status: 'healthy' | 'unavailable'
  error?: string
}

export interface PendingGrantRequest {
  milestoneId: string
  kind?: 'command' | 'touch-path' | 'worker-deny' | 'egress'
  command: string
}

export interface PendingQuestion {
  questionId: string
  role: string
  text: string
  options?: string[]
  runId?: string
  featureId?: string
  milestoneId?: string
}

export interface MissionState {
  mission: {
    id?: string
    status: string
    goal: string
  }
  pendingGrantRequest?: PendingGrantRequest
  pendingQuestions?: PendingQuestion[]
}

export interface KranzApi {
  listMissions(): Promise<MissionSummary[]>
  missionState(id: string): Promise<MissionState>
  approveGrant(id: string, command: string): Promise<void>
  denyGrant(id: string, command: string, reason: string): Promise<void>
  answerQuestion(id: string, questionId: string, answer: string, option: number): Promise<void>
}

export interface MissionCard {
  summary: MissionSummary
  state?: MissionState
  loadError?: string
}

export type DecisionAction =
  | {
      kind: 'grant-approve' | 'grant-deny'
      label: string
      command: string
    }
  | {
      kind: 'question-answer'
      label: string
      questionId: string
      answer: string
      option: number
    }
