import type { KranzApi, MissionState, MissionSummary, RepoSummary } from './contracts'

const TOKEN_KEY = 'kranz-even-g2-mutation-token'

export class ApiError extends Error {
  constructor(
    readonly status: number,
    message: string,
  ) {
    super(message)
    this.name = 'ApiError'
  }
}

export function setMutationToken(token: string): void {
  try {
    sessionStorage.setItem(TOKEN_KEY, token.trim())
  } catch {
    // Privacy modes can disable storage. The companion UI will ask again.
  }
}

export function mutationToken(): string | null {
  try {
    const token = sessionStorage.getItem(TOKEN_KEY)
    return token && token.trim() !== '' ? token : null
  } catch {
    return null
  }
}

async function responseError(response: Response): Promise<ApiError> {
  let detail = `${response.status} ${response.statusText}`
  try {
    const value = (await response.json()) as { error?: unknown }
    if (typeof value.error === 'string' && value.error !== '') detail = value.error
  } catch {
    // Preserve the HTTP fallback for non-JSON proxy/server failures.
  }
  return new ApiError(response.status, detail)
}

export class HttpKranzApi implements KranzApi {
  private selectedRepoPromise: Promise<RepoSummary> | null = null

  constructor(
    private readonly fetchImpl: typeof fetch = fetch,
    private readonly base = '',
    private readonly preferredRepoId?: string,
  ) {}

  private async json<T>(path: string, init?: RequestInit): Promise<T> {
    const headers = new Headers(init?.headers)
    const token = mutationToken()
    if (token !== null) headers.set('x-kranz-token', token)
    const response = await this.fetchImpl(`${this.base}${path}`, { ...init, headers })
    if (!response.ok) throw await responseError(response)
    const contentType = response.headers.get('content-type') ?? ''
    if (!contentType.includes('application/json')) {
      throw new ApiError(response.status, 'Kranz returned a non-JSON response')
    }
    return (await response.json()) as T
  }

  private async post(path: string, body: unknown): Promise<void> {
    await this.json(path, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify(body),
    })
  }

  private selectedRepo(): Promise<RepoSummary> {
    if (this.selectedRepoPromise === null) {
      const discovery = this.json<RepoSummary[]>('/api/repos').then((repos) => {
        const healthy = repos.filter((repo) => repo.status === 'healthy')
        const preferred = this.preferredRepoId
          ? repos.find((repo) => repo.id === this.preferredRepoId)
          : undefined
        if (this.preferredRepoId && preferred?.status !== 'healthy') {
          throw new Error(`Requested Kranz repository '${this.preferredRepoId}' is unavailable`)
        }
        const pinned = healthy.filter((repo) => repo.pinned)
        const selected =
          preferred ??
          healthy.find((repo) => repo.isDefault) ??
          (healthy.length === 1 ? healthy[0] : undefined) ??
          (pinned.length === 1 ? pinned[0] : undefined)
        if (!selected) {
          if (healthy.length === 0) throw new Error('No healthy Kranz repository is available')
          throw new Error('Multiple healthy Kranz repositories; add ?repo=<id> to choose one')
        }
        return selected
      })
      this.selectedRepoPromise = discovery.catch((error: unknown) => {
        this.selectedRepoPromise = null
        throw error
      })
    }
    return this.selectedRepoPromise
  }

  private async repoPrefix(): Promise<string> {
    return `/api/repos/${encodeURIComponent((await this.selectedRepo()).id)}`
  }

  async listMissions(): Promise<MissionSummary[]> {
    const repo = await this.selectedRepo()
    const missions = await this.json<MissionSummary[]>(
      `/api/repos/${encodeURIComponent(repo.id)}/missions`,
    )
    return missions.map((mission) => ({
      ...mission,
      repoId: repo.id,
      repoDisplayName: repo.displayName,
    }))
  }

  async missionState(id: string): Promise<MissionState> {
    return this.json(`${await this.repoPrefix()}/missions/${encodeURIComponent(id)}/state`)
  }

  async approveGrant(id: string, command: string): Promise<void> {
    return this.post(`${await this.repoPrefix()}/missions/${encodeURIComponent(id)}/grant/approve`, {
      command,
    })
  }

  async denyGrant(id: string, command: string, reason: string): Promise<void> {
    return this.post(`${await this.repoPrefix()}/missions/${encodeURIComponent(id)}/grant/deny`, {
      command,
      reason,
    })
  }

  async answerQuestion(
    id: string,
    questionId: string,
    answer: string,
    option: number,
  ): Promise<void> {
    return this.post(`${await this.repoPrefix()}/missions/${encodeURIComponent(id)}/question/answer`, {
      questionId,
      answer,
      option,
    })
  }
}
