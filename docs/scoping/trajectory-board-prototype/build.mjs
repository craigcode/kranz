// Assembles the prototype into one page: the template, the seven scripts and
// whichever kit models are present under kit/. No dependencies.
//
//   node build.mjs        writes dist/board.html
import { existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';

const require = createRequire(import.meta.url);
const ROOT = fileURLToPath(new URL('.', import.meta.url));

// scene.js is loaded only to read the list of kit models it places.
globalThis.KB = {};
require('./src/scene.js');
const kit = {};
const missing = [];
let bytes = 0;
for (const name of globalThis.KB.KIT_USED) {
  const file = join(ROOT, 'kit', name + '.glb');
  if (!existsSync(file)) {
    missing.push(name);
    continue;
  }
  const buf = readFileSync(file);
  bytes += buf.length;
  kit[name] = buf.toString('base64');
}

const js = ['sim', 'kranz', 'fold', 'profile', 'scene', 'chart', 'ui']
  .map((f) => readFileSync(join(ROOT, 'src', f + '.js'), 'utf8'))
  .join('\n');
if (/<\/script/i.test(js)) throw new Error('script would close early');

let html = readFileSync(join(ROOT, 'src', 'template.html'), 'utf8');
html = html.replace('/*__KIT__*/{}', () => JSON.stringify(kit)).replace('/*__JS__*/', () => js);

const head =
  '<!doctype html><html lang="en"><head><meta charset="utf-8">' +
  '<meta name="viewport" content="width=device-width, initial-scale=1, viewport-fit=cover">' +
  '<style>:root{color-scheme:light;padding-top:env(safe-area-inset-top,0px);padding-bottom:env(safe-area-inset-bottom,0px)}' +
  'body{margin:0;font:14px system-ui,sans-serif}img{max-width:100%}[hidden]{display:none!important}</style></head><body>';
mkdirSync(join(ROOT, 'dist'), { recursive: true });
writeFileSync(join(ROOT, 'dist', 'board.html'), head + html + '</body></html>');

console.log(
  `built dist/board.html: ${(html.length / 1000).toFixed(0)} KB, ` +
    `${Object.keys(kit).length} kit models (${(bytes / 1000).toFixed(0)} KB raw)` +
    (missing.length ? `, ${missing.length} not found under kit/ and left out` : ''),
);
