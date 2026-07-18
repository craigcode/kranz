import { cp, mkdir, readdir, readFile, rm } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const appRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const source = path.join(appRoot, 'dist');
const destination = path.resolve(appRoot, '../../crates/cli/assets/dashboard/dist');

async function filesUnder(root, relative = '') {
  const entries = await readdir(path.join(root, relative), { withFileTypes: true });
  const files = [];
  for (const entry of entries) {
    const child = path.join(relative, entry.name);
    if (entry.isDirectory()) files.push(...(await filesUnder(root, child)));
    else if (entry.isFile()) files.push(child);
  }
  return files.sort();
}

async function check() {
  const [sourceFiles, destinationFiles] = await Promise.all([
    filesUnder(source),
    filesUnder(destination),
  ]);
  const sourceSet = new Set(sourceFiles);
  const destinationSet = new Set(destinationFiles);
  const differences = new Set([
    ...sourceFiles.filter((file) => !destinationSet.has(file)),
    ...destinationFiles.filter((file) => !sourceSet.has(file)),
  ]);
  for (const file of sourceFiles) {
    if (!destinationFiles.includes(file)) continue;
    const [built, embedded] = await Promise.all([
      readFile(path.join(source, file)),
      readFile(path.join(destination, file)),
    ]);
    if (!built.equals(embedded)) differences.add(file);
  }
  if (differences.size > 0) {
    console.error('embedded dashboard is stale; run `npm run sync-embedded` after building:');
    for (const file of [...differences].sort()) console.error(`  ${file}`);
    process.exitCode = 1;
  }
}

if (process.argv.includes('--check')) {
  await check();
} else {
  await rm(destination, { recursive: true, force: true });
  await mkdir(path.dirname(destination), { recursive: true });
  await cp(source, destination, { recursive: true });
  console.log(`synced ${source} to ${destination}`);
}
