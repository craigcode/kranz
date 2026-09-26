// Keep in sync with engine::presentation. Escaping is for display; the engine
// independently refuses ambiguous approval bytes and binds the original action.
// oxlint-disable-next-line no-control-regex -- Detecting display controls is the purpose of this expression.
const ambiguous = /[\u0000-\u0008\u000b-\u001f\u007f-\u009f\u00ad\u061c\u180e\u200b-\u200f\u2028-\u202e\u2060-\u206f\ufeff\ufff9-\ufffb\u{e0001}\u{e0020}-\u{e007f}]/u;

export function visible(text: string): string {
  return Array.from(text, (c) => ambiguous.test(c)
    ? Array.from({ length: c.length }, (_, i) => `\\u${c.charCodeAt(i).toString(16).padStart(4, '0')}`).join('')
    : c).join('');
}

export function hasAmbiguous(value: unknown): boolean {
  if (typeof value === 'string') return ambiguous.test(value);
  if (Array.isArray(value)) return value.some(hasAmbiguous);
  if (value && typeof value === 'object') return Object.entries(value).some(([k, v]) => hasAmbiguous(k) || hasAmbiguous(v));
  return false;
}
