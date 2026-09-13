import { copyFile, readFile, writeFile } from 'node:fs/promises'

await copyFile('../../LICENSE', 'dist/LICENSE')
const starter = await readFile('NOTICE.md', 'utf8')
const sdk = await readFile('node_modules/@evenrealities/even_hub_sdk/LICENSE', 'utf8')
await writeFile('dist/THIRD-PARTY-NOTICES.txt', `${starter}\n\n@evenrealities/even_hub_sdk 0.0.14\n\n${sdk}`)
