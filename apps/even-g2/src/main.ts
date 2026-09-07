import {
  CreateStartUpPageContainer,
  OsEventTypeList,
  RebuildPageContainer,
  TextContainerProperty,
  TextContainerUpgrade,
  waitForEvenAppBridge,
} from '@evenrealities/even_hub_sdk'
import './styles.css'
import { HttpKranzApi, mutationToken, setMutationToken } from './api'
import { KranzGlassesController, type DisplayFrame, type Input } from './controller'
import { DemoKranzApi } from './demo'

const demo = new URLSearchParams(window.location.search).get('demo') === '1'
const preferredRepoId = new URLSearchParams(window.location.search).get('repo') ?? undefined

const app = document.querySelector<HTMLElement>('#app')
if (!app) throw new Error('missing #app')
app.innerHTML = `
  <section class="shell">
    <header>
      <div>
        <p class="eyebrow">EVEN G2 CLIENT</p>
        <h1>Kranz Mission Control</h1>
      </div>
      <span class="mode">${demo ? 'DEMO DATA' : 'LIVE API'}</span>
    </header>
    <pre id="mirror" aria-live="polite"></pre>
    <div class="controls">
      <button data-input="previous">Previous</button>
      <button data-input="select" class="primary">Select</button>
      <button data-input="next">Next</button>
      <button data-input="back">Back</button>
      <button id="refresh">Refresh</button>
    </div>
    <form id="token-form" ${demo ? 'hidden' : ''}>
      <label for="token">Serve mutation token</label>
      <div class="token-row">
        <input id="token" name="token" type="password" autocomplete="off" placeholder="Paste token" />
        <button type="submit">Use token</button>
      </div>
      <p>Stored for this WebView session only. Never placed in the URL.</p>
    </form>
    <footer>Swipe or use the R1 ring to move · tap to select · double-tap to exit</footer>
  </section>
`

function requiredElement<T extends Element>(selector: string): T {
  const element = document.querySelector<T>(selector)
  if (!element) throw new Error(`missing ${selector}`)
  return element
}

const mirrorElement = requiredElement<HTMLElement>('#mirror')

const bridge = await waitForEvenAppBridge()
let pageCreated = false
let rendering: Promise<unknown> = Promise.resolve()
let lastBridgeWriteAt = 0
const MIN_BRIDGE_WRITE_GAP_MS = 150

async function paceBridgeWrites(): Promise<void> {
  const remaining = MIN_BRIDGE_WRITE_GAP_MS - (Date.now() - lastBridgeWriteAt)
  if (remaining > 0) await new Promise((resolve) => window.setTimeout(resolve, remaining))
}

function frameText(frame: DisplayFrame): string {
  return `${frame.title}\n${frame.body}\n${frame.footer}`.trim()
}

async function renderGlasses(frame: DisplayFrame): Promise<void> {
  const content = frameText(frame)
  mirrorElement.textContent = content
  rendering = rendering.catch(() => undefined).then(async () => {
    await paceBridgeWrites()
    if (!pageCreated) {
      const main = new TextContainerProperty({
        xPosition: 0,
        yPosition: 0,
        width: 576,
        height: 288,
        borderWidth: 0,
        borderColor: 0,
        paddingLength: 8,
        containerID: 1,
        containerName: 'kranz',
        content,
        textColor: 4,
        isEventCapture: 1,
      })
      const result = await bridge.createStartUpPageContainer(
        new CreateStartUpPageContainer({ containerTotalNum: 1, textObject: [main] }),
      )
      if (result !== 0) {
        // Vite reloads can leave the simulator's existing page alive while a
        // fresh WebView bridge starts. Rebuild the same validated page rather
        // than turning every hot reload into a simulator restart.
        const rebuilt = await bridge.rebuildPageContainer(
          new RebuildPageContainer({ containerTotalNum: 1, textObject: [main] }),
        )
        if (!rebuilt) throw new Error(`createStartUpPageContainer failed (${result})`)
      }
      pageCreated = true
      lastBridgeWriteAt = Date.now()
      return
    }
    const updated = await bridge.textContainerUpgrade(
      new TextContainerUpgrade({
        containerID: 1,
        containerName: 'kranz',
        content,
        textColor: 4,
      }),
    )
    if (!updated) throw new Error('Glasses display update failed; refresh before deciding')
    lastBridgeWriteAt = Date.now()
  })
  await rendering
}

const controller = new KranzGlassesController(
  demo ? new DemoKranzApi() : new HttpKranzApi(fetch, '', preferredRepoId),
  renderGlasses,
)

async function loadMissions(): Promise<void> {
  if (!demo && mutationToken() === null) {
    await renderGlasses({
      title: 'KRANZ · AUTH',
      body: 'Paste the serve mutation token in the phone companion to load missions.',
      footer: 'token stays in this session',
      mode: 'loading',
    })
    document.querySelector<HTMLInputElement>('#token')?.focus()
    return
  }
  await controller.refresh()
}

function eventType(envelope?: { eventType?: OsEventTypeList }): OsEventTypeList | null {
  if (!envelope) return null
  return envelope.eventType ?? OsEventTypeList.CLICK_EVENT
}

function dispatch(input: Input): void {
  void controller.input(input).catch((error: unknown) => console.error(error))
}

const unsubscribe = bridge.onEvenHubEvent((event) => {
  const sys = eventType(event.sysEvent)
  const text = eventType(event.textEvent)
  if (sys === OsEventTypeList.DOUBLE_CLICK_EVENT || text === OsEventTypeList.DOUBLE_CLICK_EVENT) {
    void bridge.shutDownPageContainer(1)
    return
  }
  if (text === OsEventTypeList.SCROLL_TOP_EVENT) {
    dispatch('previous')
    return
  }
  if (text === OsEventTypeList.SCROLL_BOTTOM_EVENT) {
    dispatch('next')
    return
  }
  if (sys === OsEventTypeList.CLICK_EVENT || text === OsEventTypeList.CLICK_EVENT) {
    dispatch('select')
    return
  }
  if (sys === OsEventTypeList.SYSTEM_EXIT_EVENT || sys === OsEventTypeList.ABNORMAL_EXIT_EVENT) {
    unsubscribe()
  }
})

for (const button of document.querySelectorAll<HTMLButtonElement>('[data-input]')) {
  button.addEventListener('click', () => dispatch(button.dataset.input as Input))
}
document.querySelector('#refresh')?.addEventListener('click', () => void loadMissions())
document.querySelector<HTMLFormElement>('#token-form')?.addEventListener('submit', (event) => {
  event.preventDefault()
  const input = document.querySelector<HTMLInputElement>('#token')
  if (input?.value.trim()) {
    setMutationToken(input.value)
    input.value = ''
    void loadMissions()
  }
})

window.addEventListener('beforeunload', unsubscribe)
await loadMissions()
