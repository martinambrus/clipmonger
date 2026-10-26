// Build: tsc (src+tests -> dist/) + flatten src output to dist/ root for
// GNOME (extension.js must sit beside metadata.json) + copy assets.
import { execFileSync } from 'node:child_process';
import { cpSync, copyFileSync, mkdirSync, readdirSync, existsSync } from 'node:fs';

execFileSync('npx', ['tsc', '-p', 'tsconfig.json'], { stdio: 'inherit' });
execFileSync('npx', ['tsc', '--noEmit', '-p', 'tsconfig.json'], { stdio: 'inherit' });
for (const f of readdirSync('dist/src')) {
  if (f.endsWith('.js')) copyFileSync(`dist/src/${f}`, `dist/${f}`);
}
copyFileSync('metadata.json', 'dist/metadata.json');
copyFileSync('stylesheet.css', 'dist/stylesheet.css');
mkdirSync('dist/schemas', { recursive: true });
for (const f of readdirSync('schemas')) copyFileSync(`schemas/${f}`, `dist/schemas/${f}`);
if (!existsSync('dist/extension.js')) throw new Error('dist/extension.js missing after build');
console.log('build ok: dist/extension.js + metadata.json + schemas/');
