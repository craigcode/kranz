import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import {
  awaitToken,
  cancelTokenPrompt,
  clearToken,
  provideToken,
  resolveToken,
  setToken,
  tokenGateSnapshot,
} from './token';

const STORAGE_KEY = 'kranz-token';

beforeEach(() => {
  clearToken();
  sessionStorage.removeItem(STORAGE_KEY);
  delete window.__KRANZ_TOKEN__;
  cancelTokenPrompt();
});

afterEach(() => {
  cancelTokenPrompt();
  clearToken();
  sessionStorage.removeItem(STORAGE_KEY);
});

describe('clearToken', () => {
  it('removes the cached and sessionStorage token so resolveToken returns null', () => {
    setToken('stale-after-restart');
    expect(resolveToken()).toBe('stale-after-restart');
    expect(sessionStorage.getItem(STORAGE_KEY)).toBe('stale-after-restart');

    clearToken();

    expect(sessionStorage.getItem(STORAGE_KEY)).toBeNull();
    expect(resolveToken()).toBeNull();
  });
});

describe('awaitToken on 401 rejection', () => {
  it('clears a stale sessionStorage token so the next paste is required', async () => {
    setToken('stale-after-restart');
    expect(sessionStorage.getItem(STORAGE_KEY)).toBe('stale-after-restart');

    const pending = awaitToken();
    expect(tokenGateSnapshot()).toEqual({ needed: true, rejected: true });
    expect(sessionStorage.getItem(STORAGE_KEY)).toBeNull();
    expect(resolveToken()).toBeNull();

    provideToken('fresh-token');
    await pending;
    expect(resolveToken()).toBe('fresh-token');
  });

  it('keeps rejected:true when a second 401 arrives after the first cleared the token', async () => {
    setToken('stale-after-restart');

    // First 401: had a token, so the gate escalates to rejected and clears it.
    const first = awaitToken();
    expect(tokenGateSnapshot()).toEqual({ needed: true, rejected: true });
    expect(resolveToken()).toBeNull();

    // Second concurrent 401 arrives with no stored token left — it must not
    // downgrade the prompt back to the generic wording.
    const second = awaitToken();
    expect(tokenGateSnapshot()).toEqual({ needed: true, rejected: true });

    // Gate resolution resets the escalation for the next round.
    provideToken('fresh-token');
    await Promise.all([first, second]);
    expect(tokenGateSnapshot()).toEqual({ needed: false, rejected: false });
  });
});
