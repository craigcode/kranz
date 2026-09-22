import { cleanup, render } from '@testing-library/react';
import { afterEach, expect, it } from 'vitest';
import { renderMarkdown } from './markdown';

afterEach(cleanup);
it('renders escaped review prose literally without creating links, markup or inline claims', () => {
  const { container } = render(<>{renderMarkdown('source\\.rs · attempt\\-1\n\n\\# forged heading\n\n\\*\\*pass\\*\\* \\`claim\\` \\<script\\> \\[link\\]\\(https://example.invalid\\)')}</>);
  expect(container.textContent).toContain('source.rs · attempt-1');
  expect(container.textContent).toContain('**pass** `claim` <script> [link](https://example.invalid)');
  expect(container.querySelector('strong, code, a, script, .md-heading')).toBeNull();
});

it('keeps escaped punctuation inside a formatted review label', () => {
  const { container } = render(<>{renderMarkdown('**Gate attempt attempt\\-1** — event #7')}</>);
  expect(container.querySelector('strong')?.textContent).toBe('Gate attempt attempt-1');
});
