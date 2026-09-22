// Minimal markdown renderer (no deps, no innerHTML — everything is React
// nodes, so untrusted transcript text cannot inject markup).
//
// Supported: ``` fenced code blocks, # headings, - / * / 1. lists, paragraphs,
// inline `code` and **bold**. Everything else renders as plain text.

import type { ReactNode } from 'react';

function renderInline(text: string, keyBase: string): ReactNode[] {
  // Consume escaped punctuation before looking for formatting. A complete
  // bold/code span can still contain escapes without losing its delimiters.
  const unescape = (value: string) => value.replace(/\\([\\`*_{}[\]<>()#+\-.!|])/g, '$1');
  const parts = text.split(/(\\[\\`*_{}[\]<>()#+\-.!|]|`(?:\\.|[^`])+`|\*\*(?:\\.|[^*])+\*\*)/g);
  return parts.map((part, i) => {
    if (/^\\[\\`*_{}[\]<>()#+\-.!|]$/.test(part)) return part.slice(1);
    if (part.startsWith('`') && part.endsWith('`') && part.length > 2) {
      return <code key={`${keyBase}-c${i}`}>{unescape(part.slice(1, -1))}</code>;
    }
    if (part.startsWith('**') && part.endsWith('**') && part.length > 4) {
      return <strong key={`${keyBase}-b${i}`}>{unescape(part.slice(2, -2))}</strong>;
    }
    return part;
  });
}

export function renderMarkdown(text: string): ReactNode {
  const lines = text.split('\n');
  const blocks: ReactNode[] = [];
  let paragraph: string[] = [];
  let list: string[] = [];
  let code: string[] | null = null;
  let key = 0;

  const flushParagraph = () => {
    if (paragraph.length === 0) return;
    const joined = paragraph.join(' ');
    blocks.push(<p key={`p${key++}`}>{renderInline(joined, `p${key}`)}</p>);
    paragraph = [];
  };

  const flushList = () => {
    if (list.length === 0) return;
    blocks.push(
      <ul key={`l${key++}`}>
        {list.map((item, i) => (
          <li key={i}>{renderInline(item, `l${key}-${i}`)}</li>
        ))}
      </ul>,
    );
    list = [];
  };

  for (const line of lines) {
    if (code !== null) {
      if (line.trimEnd().startsWith('```')) {
        blocks.push(
          <pre key={`f${key++}`}>
            <code>{code.join('\n')}</code>
          </pre>,
        );
        code = null;
      } else {
        code.push(line);
      }
      continue;
    }
    const trimmed = line.trim();
    if (trimmed.startsWith('```')) {
      flushParagraph();
      flushList();
      code = [];
      continue;
    }
    if (trimmed === '') {
      flushParagraph();
      flushList();
      continue;
    }
    const heading = /^(#{1,4})\s+(.*)$/.exec(trimmed);
    if (heading) {
      flushParagraph();
      flushList();
      blocks.push(
        <p key={`h${key++}`} className="md-heading">
          {renderInline(heading[2], `h${key}`)}
        </p>,
      );
      continue;
    }
    const listItem = /^(?:[-*]|\d+\.)\s+(.*)$/.exec(trimmed);
    if (listItem) {
      flushParagraph();
      list.push(listItem[1]);
      continue;
    }
    flushList();
    paragraph.push(trimmed);
  }

  // Unterminated fence: render what we have.
  if (code !== null && code.length > 0) {
    blocks.push(
      <pre key={`f${key++}`}>
        <code>{code.join('\n')}</code>
      </pre>,
    );
  }
  flushParagraph();
  flushList();

  return <div className="md">{blocks}</div>;
}
