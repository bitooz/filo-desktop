// Copies Lucide icons (ISC licence, THIRD_PARTY_NOTICES.md) into assets/icons as SVG files:
// `node apps/desktop/scripts/lucide-icons.mjs <name>…` from the root of Filo's phone app repository (not public),
// whose node_modules has lucide-react-native.
import { readFileSync, writeFileSync, existsSync } from 'node:fs';
const dir = 'node_modules/lucide-react-native/dist/esm/icons/';
const names = process.argv.slice(2);
const missing = [];
for (const name of names) {
  const file = dir + name + '.mjs';
  if (!existsSync(file)) { missing.push(name); continue; }
  const src = readFileSync(file, 'utf8');
  const m = src.match(/createLucideIcon\("[^"]+",\s*(\[[\s\S]*\])\);/);
  const nodes = new Function('return ' + m[1])();
  const body = nodes.map(([tag, attrs]) => '<' + tag + ' ' + Object.entries(attrs).filter(([k]) => k !== 'key').map(([k, v]) => `${k}="${v}"`).join(' ') + '/>').join('');
  writeFileSync(`apps/desktop/assets/icons/${name}.svg`, `<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">${body}</svg>\n`);
}
console.log('missing:', missing.join(' '));
