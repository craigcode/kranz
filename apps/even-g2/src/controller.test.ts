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
