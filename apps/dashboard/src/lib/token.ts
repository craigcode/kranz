// Mutation-token resolution + the 401 gate (docs/protocol.md "Authority:
// mutation token"). Every POST needs the per-serve session token in the
// x-kranz-token header. Resolution order:
//
//   1. window.__KRANZ_TOKEN__     — injected by the embedding Tauri shell
//   2. location.hash "#token=<t>" — appended by `kranz serve --open`
//                                   (stripped from the hash after reading)
//   3. sessionStorage 'kranz-token'
//   4. null — the TokenPrompt paste field is the last resort
//
// Whenever a token is found it is persisted to sessionStorage so reloads
// within the tab keep working after the hash was stripped.
//
// The gate: when a POST comes back 401, api.ts calls awaitToken() and
// retries once the user pastes a token into <TokenPrompt/>. The prompt
// subscribes to this module via useSyncExternalStore.

declare global {
  interface Window {
    __KRANZ_TOKEN__?: string;
  }
}

const STORAGE_KEY = 'kranz-token';

let cached: string | null = null;

/** Extract `token=<t>` from the location hash and strip it, keeping any
 *  routing part (e.g. "#/m/x&token=t" → "#/m/x"). */
function takeTokenFromHash(): string | null {
  const hash = window.location.hash;
  const match = /(^#|[#&?])token=([^&]+)/.exec(hash);
  if (!match) return null;
  const token = decodeURIComponent(match[2]);
  const stripped = hash.replace(/(^#|[#&?])token=[^&]+/, (_, sep: string) => (sep === '#' ? '#' : ''));
  const clean = stripped === '#' ? '' : stripped;
  // replaceState avoids a hashchange event (which would re-route the app).
  history.replaceState(null, '', window.location.pathname + window.location.search + clean);
  return token;
}

function readSessionStorage(): string | null {
  try {
    return sessionStorage.getItem(STORAGE_KEY);
  } catch {
    return null; // storage can be unavailable (privacy modes)
  }
}

/** Persist + cache a token (from any source, including the paste field). */
export function setToken(token: string): void {
  cached = token;
  try {
    sessionStorage.setItem(STORAGE_KEY, token);
  } catch {
    /* keep the in-memory copy */
  }
}

/** Drop the cached + sessionStorage token (e.g. after a 401 rejection so a
 *  stale post-restart token cannot keep failing silently). */
export function clearToken(): void {
  cached = null;
  try {
    sessionStorage.removeItem(STORAGE_KEY);
  } catch {
    /* ignore storage failures */
  }
}

/** Resolve the current mutation token, or null when we have none. */
export function resolveToken(): string | null {
  if (cached !== null) return cached;
  const injected = window.__KRANZ_TOKEN__;
  if (injected !== undefined && injected !== '') {
    setToken(injected);
    return injected;
  }
  const fromHash = takeTokenFromHash();
  if (fromHash !== null && fromHash !== '') {
    setToken(fromHash);
    return fromHash;
  }
  const stored = readSessionStorage();
  if (stored !== null && stored !== '') {
    cached = stored;
    return stored;
  }
  return null;
}

// ---------------------------------------------------------------------------
// 401 gate — pending mutations wait here for a pasted token
// ---------------------------------------------------------------------------

export interface TokenGateState {
  /** True while at least one mutation is blocked on a token. */
  needed: boolean;
  /** True when the last attempt HAD a token and the server rejected it. */
  rejected: boolean;
}

interface Waiter {
  resolve: () => void;
  reject: (err: Error) => void;
}

let waiters: Waiter[] = [];
let rejectedLast = false;
let snapshot: TokenGateState = { needed: false, rejected: false };
const listeners = new Set<() => void>();

function notify(): void {
  snapshot = { needed: waiters.length > 0, rejected: rejectedLast };
  for (const l of listeners) l();
}

/** useSyncExternalStore subscribe function. */
export function subscribeTokenGate(listener: () => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

/** useSyncExternalStore snapshot function (stable reference between changes). */
export function tokenGateSnapshot(): TokenGateState {
  return snapshot;
}

/**
 * Called by api.ts after a 401: blocks until the user provides a token via
 * provideToken() (resolve → caller retries) or cancels (reject). Clears any
 * cached token first so a stale post-restart value cannot be retried.
 */
export function awaitToken(): Promise<void> {
  rejectedLast = resolveToken() !== null;
  if (rejectedLast) clearToken();
  return new Promise<void>((resolve, reject) => {
    waiters.push({ resolve, reject });
    notify();
  });
}

/** The paste field submits here: store the token, release every waiter. */
export function provideToken(token: string): void {
  setToken(token.trim());
  const pending = waiters;
  waiters = [];
  notify();
  for (const w of pending) w.resolve();
}

/** Dismiss the prompt: every blocked mutation fails with a clear error. */
export function cancelTokenPrompt(): void {
  const pending = waiters;
  waiters = [];
  rejectedLast = false;
  notify();
  for (const w of pending) w.reject(new Error('mutation cancelled — token required'));
}
