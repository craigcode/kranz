// Compact bar shown when a mutation was attempted without a (valid) mutation
// token — docs/protocol.md "Authority: mutation token". Pasting the token
// stores it (sessionStorage) and retries every blocked request; dismissing
// fails them with a clear error. Rendered app-wide (fixed to the bottom).

import { useState, useSyncExternalStore } from 'react';
import {
  cancelTokenPrompt,
  provideToken,
  subscribeTokenGate,
  tokenGateSnapshot,
} from '../lib/token';

export function TokenPrompt() {
  const gate = useSyncExternalStore(subscribeTokenGate, tokenGateSnapshot);
  const [value, setValue] = useState('');

  if (!gate.needed) return null;

  const submit = () => {
    const token = value.trim();
    if (token === '') return;
    setValue('');
    provideToken(token); // stores + retries the blocked mutation(s)
  };

  return (
    <div className="token-prompt" role="alertdialog" aria-label="Mutation token required">
      <span className="token-prompt-text">
        {gate.rejected ? (
          <>
            <strong>token rejected</strong> — paste the mutation token printed by{' '}
            <code>kranz serve</code>
          </>
        ) : (
          <>
            paste the mutation token printed by <code>kranz serve</code>
          </>
        )}
      </span>
      <input
        type="password"
        className="token-prompt-input mono"
        aria-label="Mutation token"
        placeholder="token"
        value={value}
        autoFocus
        onChange={(e) => setValue(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === 'Enter') submit();
          if (e.key === 'Escape') cancelTokenPrompt();
        }}
      />
      <button
        type="button"
        className="btn-small btn-primary"
        disabled={value.trim() === ''}
        onClick={submit}
      >
        Use token
      </button>
      <button type="button" className="btn-small" onClick={cancelTokenPrompt}>
        Cancel
      </button>
    </div>
  );
}
