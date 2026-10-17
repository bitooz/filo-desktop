// Runs inside Filo's phone app repository (not public), whose palettes it reads; assets/themes.json is committed.
import { families, builtInFamilyNames, createFamily } from '@/theme/palettes';
// Resolved palettes for the desktop app (assets/themes.json), less the roles it doesn't draw with.
const UNUSED = ['shadow', 'userBubbleAccent'];
const roles = (colors?: Record<string, string>) =>
  colors && Object.fromEntries(Object.entries(colors).filter(([role]) => !UNUSED.includes(role)));
const out: Record<string, unknown> = {};
for (const name of builtInFamilyNames) {
  const f = families[name];
  const high = createFamily(f.seed, 'high');
  out[name] = {
    label: f.label,
    description: f.description,
    standard: { light: roles(f.light?.colors), dark: roles(f.dark?.colors) },
    high: { light: roles(high.light?.colors), dark: roles(high.dark?.colors) },
  };
}
console.log(JSON.stringify(out, null, 2));
