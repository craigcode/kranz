import type {
  DecisionAction,
  KranzApi,
  MissionCard,
  MissionState,
  PendingQuestion,
} from './contracts'

export type ControllerMode =
  | 'loading'
  | 'missions'
  | 'decision'
  | 'confirm'
  | 'submitting'
  | 'result'
  | 'error'

export interface DisplayFrame {
  title: string
  body: string
  footer: string
  mode: ControllerMode
}

export type Input = 'next' | 'previous' | 'select' | 'back'
export type FrameListener = (frame: DisplayFrame) => void | Promise<void>

const MAX_CARDS = 8

function text(value: string, max: number): string {
  const normalized = value.replace(/\s+/g, ' ').trim()
  return normalized.length <= max ? normalized : `${normalized.slice(0, max - 1)}…`
}

function openQuestion(state?: MissionState): PendingQuestion | undefined {
  return state?.pendingQuestions?.[0]
}

export function actionsFor(card: MissionCard): DecisionAction[] {
  const question = openQuestion(card.state)
  if (question && question.options && question.options.length > 0) {
    return question.options.map((answer, option) => ({
      kind: 'question-answer' as const,
      label: answer,
      questionId: question.questionId,
      answer,
      option,
    }))
  }
  const grant = card.state?.pendingGrantRequest
  if (grant) {
    return [
      { kind: 'grant-approve', label: 'Approve grant', command: grant.command },
      { kind: 'grant-deny', label: 'Deny grant', command: grant.command },
    ]
  }
  return []
}

function needsInput(card: MissionCard): boolean {
  return Boolean(openQuestion(card.state) || card.state?.pendingGrantRequest)
}

function priority(card: MissionCard): number {
  if (needsInput(card)) return 0
  if (['executing', 'running', 'blocked'].includes(card.summary.status)) return 1
  if (card.summary.status === 'complete' && card.summary.error === undefined) return 2
  return 3
}

function summaryPriority(status: string): number {
  if (status === 'blocked') return 0
  if (['executing', 'running', 'approved', 'queued'].includes(status)) return 1
  if (status === 'complete') return 2
  return 3
}

function details(card: MissionCard, action?: DecisionAction): string {
  const question = openQuestion(card.state)
  if (question) {
    if (!question.options || question.options.length === 0) {
      return `QUESTION\n${text(question.text, 150)}\n\nFree-text answer required. Use dashboard or Slack.`
    }
    return `QUESTION\n${text(question.text, 135)}\n\n> ${text(action?.label ?? '', 54)}`
  }
  const grant = card.state?.pendingGrantRequest
  if (grant) {
    const kind = (grant.kind ?? 'command').replace('-', ' ')
    return `${kind.toUpperCase()} GRANT\n${text(grant.command, 145)}\n\n> ${action?.label ?? ''}`
  }
  if (card.loadError) return `STATE UNAVAILABLE\n${text(card.loadError, 170)}`
  return `${card.summary.status.toUpperCase()}\n${text(card.summary.goal, 180)}`
}

export class KranzGlassesController {
  private mode: ControllerMode = 'loading'
  private cards: MissionCard[] = []
  private cardIndex = 0
  private actionIndex = 0
  private result = ''
  private error = ''

  constructor(
    private readonly api: KranzApi,
    private readonly listener: FrameListener,
  ) {}

  currentFrame(): DisplayFrame {
    if (this.mode === 'loading') {
      return { title: 'KRANZ', body: 'Loading missions…', footer: '', mode: this.mode }
    }
    if (this.mode === 'error') {
      return {
        title: 'KRANZ · ERROR',
        body: text(this.error, 210),
        footer: 'tap: retry',
        mode: this.mode,
      }
    }
    if (this.mode === 'result') {
      return {
        title: 'KRANZ · SENT',
        body: this.result,
        footer: 'tap: refresh',
        mode: this.mode,
      }
    }
    if (this.cards.length === 0) {
      return {
        title: 'KRANZ',
        body: 'No missions yet.',
        footer: 'tap: refresh',
        mode: this.mode,
      }
    }
    const card = this.cards[this.cardIndex]
    if (!card) throw new Error('selected mission is missing')
    const actions = actionsFor(card)
    const action = actions[this.actionIndex]
    if (this.mode === 'missions') {
      const attention = this.cards.filter(needsInput).length
      return {
        title: `KRANZ · ${this.cardIndex + 1}/${this.cards.length}${attention ? ` · ${attention} NEED YOU` : ''}`,
        body: `${card.summary.status.toUpperCase()}\n${text(card.summary.goal, 175)}\n\n${card.summary.id}`,
        footer: 'swipe: missions · tap: open',
        mode: this.mode,
      }
    }
    if (this.mode === 'confirm' || this.mode === 'submitting') {
      return {
        title: this.mode === 'submitting' ? 'KRANZ · SENDING' : 'KRANZ · CONFIRM',
        body: action
          ? `${text(action.label, 64)}\n${text(card.summary.repoDisplayName ?? card.summary.repoId ?? 'repository', 38)}\n\n${text(card.summary.goal, 110)}`
          : 'Nothing actionable on this mission.',
        footer: this.mode === 'submitting' ? 'please wait' : 'tap: SEND · swipe: cancel',
        mode: this.mode,
      }
    }
    return {
      title: `KRANZ · ${text(card.summary.repoDisplayName ?? card.summary.repoId ?? '', 22)}`,
      body: details(card, action),
      footer: actions.length > 0 ? 'swipe: choice · tap: review' : 'tap: missions',
      mode: this.mode,
    }
  }

  private async render(): Promise<void> {
    await this.listener(this.currentFrame())
  }

  async refresh(): Promise<void> {
    this.mode = 'loading'
    await this.render()
    try {
      const summaries = (await this.api.listMissions())
        .filter((mission) => mission.status !== 'deleted')
        .sort((left, right) => {
          const rank = summaryPriority(left.status) - summaryPriority(right.status)
          if (rank !== 0) return rank
          return (right.createdAt ?? '').localeCompare(left.createdAt ?? '')
        })
        .slice(0, MAX_CARDS)
      const cards = await Promise.all(
        summaries.map(async (summary): Promise<MissionCard> => {
          try {
            return { summary, state: await this.api.missionState(summary.id) }
          } catch (error) {
            return {
              summary,
              loadError: error instanceof Error ? error.message : String(error),
            }
          }
        }),
      )
      this.cards = cards.sort((left, right) => {
        const rank = priority(left) - priority(right)
        if (rank !== 0) return rank
        return (right.summary.createdAt ?? '').localeCompare(left.summary.createdAt ?? '')
      })
      this.cardIndex = 0
      this.actionIndex = 0
      this.mode = 'missions'
    } catch (error) {
      this.error = error instanceof Error ? error.message : String(error)
      this.mode = 'error'
    }
    await this.render()
  }

  async input(input: Input): Promise<void> {
    if (this.mode === 'loading' || this.mode === 'submitting') return
    if (this.mode === 'error' || this.mode === 'result' || this.cards.length === 0) {
      if (input === 'select') await this.refresh()
      return
    }
    if (this.mode === 'confirm' && (input === 'next' || input === 'previous' || input === 'back')) {
      this.mode = 'decision'
      await this.render()
      return
    }
    if (input === 'back') {
      this.mode = 'missions'
      this.actionIndex = 0
      await this.render()
      return
    }
    if (input === 'next' || input === 'previous') {
      const delta = input === 'next' ? 1 : -1
      if (this.mode === 'missions') {
        this.cardIndex = (this.cardIndex + delta + this.cards.length) % this.cards.length
        this.actionIndex = 0
      } else {
        const card = this.cards[this.cardIndex]
        const count = card ? actionsFor(card).length : 0
        if (count > 0) this.actionIndex = (this.actionIndex + delta + count) % count
      }
      await this.render()
      return
    }
    if (this.mode === 'missions') {
      this.mode = 'decision'
      await this.render()
      return
    }
    if (this.mode === 'decision') {
      const card = this.cards[this.cardIndex]
      if (!card || actionsFor(card).length === 0) {
        this.mode = 'missions'
      } else {
        this.mode = 'confirm'
      }
      await this.render()
      return
    }
    if (this.mode === 'confirm') await this.submit()
  }

  private async submit(): Promise<void> {
    const card = this.cards[this.cardIndex]
    const action = card ? actionsFor(card)[this.actionIndex] : undefined
    if (!card || !action) {
      this.mode = 'decision'
      await this.render()
      return
    }
    this.mode = 'submitting'
    await this.render()
    try {
      if (action.kind === 'question-answer') {
        await this.api.answerQuestion(
          card.summary.id,
          action.questionId,
          action.answer,
          action.option,
        )
      } else if (action.kind === 'grant-approve') {
        await this.api.approveGrant(card.summary.id, action.command)
      } else {
        await this.api.denyGrant(
          card.summary.id,
          action.command,
          'Denied from Even G2 Mission Control',
        )
      }
      this.result = `${text(action.label, 140)}\n\n${card.summary.id}`
      this.mode = 'result'
    } catch (error) {
      this.error = error instanceof Error ? error.message : String(error)
      this.mode = 'error'
    }
    await this.render()
  }
}
