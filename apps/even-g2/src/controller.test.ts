import { describe, expect, it, vi } from 'vitest'
import type { KranzApi, MissionState, MissionSummary } from './contracts'
import { KranzGlassesController, actionsFor, type DisplayFrame } from './controller'

function fixtureApi(state: MissionState): KranzApi {
  const summary: MissionSummary = {
    id: 'm-test',
    status: state.mission.status,
    goal: state.mission.goal,
  }
  return {
    listMissions: vi.fn().mockResolvedValue([summary]),
    missionState: vi.fn().mockResolvedValue(state),
    approveGrant: vi.fn().mockResolvedValue(undefined),
    denyGrant: vi.fn().mockResolvedValue(undefined),
    answerQuestion: vi.fn().mockResolvedValue(undefined),
  }
}

describe('KranzGlassesController', () => {
  it('requires a review screen and a separate confirmation tap before granting', async () => {
    const api = fixtureApi({
      mission: { status: 'blocked', goal: 'Ship safely' },
      pendingGrantRequest: { milestoneId: 'ms-1', kind: 'egress', command: 'example.com:443' },
    })
    let frame: DisplayFrame | undefined
    const controller = new KranzGlassesController(api, (next) => {
      frame = next
    })
    await controller.refresh()

    await controller.input('select')
    expect(frame?.mode).toBe('decision')
    expect(api.approveGrant).not.toHaveBeenCalled()

    await controller.input('select')
    expect(frame?.mode).toBe('confirm')
    expect(api.approveGrant).not.toHaveBeenCalled()

    await controller.input('select')
    expect(api.approveGrant).toHaveBeenCalledWith('m-test', 'example.com:443')
    expect(frame?.mode).toBe('result')
  })

  it('cancels confirmation on a swipe', async () => {
    const api = fixtureApi({
      mission: { status: 'blocked', goal: 'Ship safely' },
      pendingGrantRequest: { milestoneId: 'ms-1', command: 'cargo publish' },
    })
    let frame: DisplayFrame | undefined
    const controller = new KranzGlassesController(api, (next) => {
      frame = next
    })
    await controller.refresh()
    await controller.input('select')
    await controller.input('select')
    await controller.input('next')

    expect(frame?.mode).toBe('decision')
    expect(api.approveGrant).not.toHaveBeenCalled()
  })

  it('answers only one of the structured choices', async () => {
    const api = fixtureApi({
      mission: { status: 'blocked', goal: 'Choose a route' },
      pendingQuestions: [
        {
          questionId: 'q-2',
          role: 'worker',
          text: 'Which route?',
          options: ['Safe', 'Fast'],
        },
      ],
    })
    const controller = new KranzGlassesController(api, () => undefined)
    await controller.refresh()
    await controller.input('select')
    await controller.input('next')
    await controller.input('select')
    await controller.input('select')

    expect(api.answerQuestion).toHaveBeenCalledWith('m-test', 'q-2', 'Fast', 1)
  })

  it('does not turn a free-text question into a wearable mutation', () => {
    expect(
      actionsFor({
        summary: { id: 'm-test', status: 'blocked', goal: 'Need context' },
        state: {
          mission: { status: 'blocked', goal: 'Need context' },
          pendingQuestions: [{ questionId: 'q-1', role: 'worker', text: 'Explain why' }],
        },
      }),
    ).toEqual([])
  })
})

describe('bounded review and delivery', () => {
  const grantState = (): MissionState => ({
    mission: { status: 'blocked', goal: 'Disposable wearable test' },
    pendingGrantRequest: { milestoneId: 'ms-1', kind: 'egress', command: 'example.com:443' },
  })

  it.each(['x'.repeat(57), 'echo safe\nrm hidden', 'example.com\u202e:443'])(
    'defers commands that cannot be fully displayed: %s', (command) => {
      expect(actionsFor({
        summary: { id: 'm-test', status: 'blocked', goal: 'test' },
        state: { ...grantState(), pendingGrantRequest: { milestoneId: 'ms-1', command } },
      })).toEqual([])
    },
  )

  it('does not approve a grant while showing an unrelated free-text question', async () => {
    const state = grantState()
    state.pendingQuestions = [{ questionId: 'q-1', role: 'worker', text: 'Explain?' }]
    const api = fixtureApi(state)
    const controller = new KranzGlassesController(api, () => undefined)
    await controller.refresh()
    await controller.input('select')
    expect(controller.currentFrame().body).toContain('Use dashboard or Slack')
    await controller.input('select')
    expect(controller.currentFrame().mode).toBe('missions')
    expect(api.approveGrant).not.toHaveBeenCalled()
  })

  it('defers a question if any offered answer would be shortened', () => {
    expect(actionsFor({
      summary: { id: 'm-test', status: 'blocked', goal: 'test' },
      state: {
        mission: { status: 'blocked', goal: 'test' },
        pendingQuestions: [{ questionId: 'q-1', role: 'worker', text: 'Choose?', options: ['Yes', 'x'.repeat(29)] }],
      },
    })).toEqual([])
  })

  it('shows the exact target and mission again on confirmation', async () => {
    const controller = new KranzGlassesController(fixtureApi(grantState()), () => undefined)
    await controller.refresh()
    await controller.input('select')
    await controller.input('select')
    expect(controller.currentFrame().body).toContain('example.com:443')
    expect(controller.currentFrame().body).toContain('m-test')
  })

  it('ignores taps until the confirmation frame has finished drawing', async () => {
    const api = fixtureApi(grantState())
    let finishDrawing: (() => void) | undefined
    const controller = new KranzGlassesController(api, (frame) => {
      if (frame.mode === 'confirm') return new Promise<void>((resolve) => { finishDrawing = resolve })
    })
    await controller.refresh()
    await controller.input('select')
    const drawing = controller.input('select')
    await controller.input('select')
    expect(api.approveGrant).not.toHaveBeenCalled()
    finishDrawing?.()
    await drawing
    expect(api.approveGrant).not.toHaveBeenCalled()
    await controller.input('select')
    expect(api.approveGrant).toHaveBeenCalledTimes(1)
  })

  it('requires a new review after a display failure', async () => {
    const api = fixtureApi(grantState())
    const controller = new KranzGlassesController(api, (frame) => {
      if (frame.mode === 'confirm') throw new Error('Bridge failed')
    })
    await controller.refresh()
    await controller.input('select')
    await controller.input('select')
    expect(controller.currentFrame().mode).toBe('error')
    await controller.input('select')
    expect(controller.currentFrame().mode).toBe('missions')
    expect(api.approveGrant).not.toHaveBeenCalled()
  })

  it('rejects a grant that changed milestone after it was reviewed', async () => {
    const api = fixtureApi(grantState())
    const controller = new KranzGlassesController(api, () => undefined)
    await controller.refresh()
    await controller.input('select')
    await controller.input('select')
    vi.mocked(api.missionState).mockResolvedValue({
      ...grantState(), pendingGrantRequest: { ...grantState().pendingGrantRequest!, milestoneId: 'ms-2' },
    })
    await controller.input('select')
    expect(api.approveGrant).not.toHaveBeenCalled()
    expect(controller.currentFrame().body).toContain('Decision changed')
  })

  it('ignores concurrent refresh and repeat taps while sending', async () => {
    const api = fixtureApi(grantState())
    let finishPost: (() => void) | undefined
    vi.mocked(api.approveGrant).mockImplementation(() => new Promise<void>((resolve) => { finishPost = resolve }))
    const controller = new KranzGlassesController(api, () => undefined)
    await controller.refresh()
    await controller.input('select')
    await controller.input('select')
    const sending = controller.input('select')
    await vi.waitFor(() => expect(api.approveGrant).toHaveBeenCalledTimes(1))
    await controller.refresh()
    await controller.input('select')
    expect(api.listMissions).toHaveBeenCalledTimes(1)
    finishPost?.()
    await sending
    expect(api.approveGrant).toHaveBeenCalledTimes(1)
    expect(controller.currentFrame().title).toBe('KRANZ · QUEUED')
  })

  it('shows a rejection and refreshes instead of replaying a failed POST', async () => {
    const api = fixtureApi(grantState())
    vi.mocked(api.approveGrant).mockRejectedValue(new Error('409 no pending grant'))
    const controller = new KranzGlassesController(api, () => undefined)
    await controller.refresh()
    await controller.input('select')
    await controller.input('select')
    await controller.input('select')
    expect(controller.currentFrame().mode).toBe('error')
    expect(controller.currentFrame().body).toContain('409')
    await controller.input('select')
    expect(api.approveGrant).toHaveBeenCalledTimes(1)
    expect(controller.currentFrame().mode).toBe('missions')
  })
})

it('keeps the proven display limits actionable without altering text', () => {
  const question = 'W'.repeat(56)
  const answer = 'W'.repeat(28)
  const actions = actionsFor({
    summary: { id: 'm-test', repoDisplayName: 'W'.repeat(28), status: 'blocked', goal: 'test' },
    state: {
      mission: { status: 'blocked', goal: 'test' },
      pendingQuestions: [{ questionId: 'q-1', role: 'worker', text: question, options: [answer] }],
    },
  })
  expect(actions).toEqual([{ kind: 'question-answer', questionId: 'q-1', label: answer, answer, option: 0 }])
})

it('requires fresh review when question wording changes before confirmation', async () => {
  const state: MissionState = {
    mission: { status: 'blocked', goal: 'test' },
    pendingQuestions: [{ questionId: 'q-1', role: 'worker', text: 'Use JSON?', options: ['Yes', 'No'] }],
  }
  const api = fixtureApi(state)
  const controller = new KranzGlassesController(api, () => undefined)
  await controller.refresh()
  await controller.input('select')
  await controller.input('select')
  vi.mocked(api.missionState).mockResolvedValue({
    ...state, pendingQuestions: [{ ...state.pendingQuestions![0]!, text: 'Publish JSON?' }],
  })
  await controller.input('select')
  expect(api.answerQuestion).not.toHaveBeenCalled()
  expect(controller.currentFrame().body).toContain('Decision changed')
})
