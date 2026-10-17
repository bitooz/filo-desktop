// Bundles the note page (apps/desktop/web/note-editor.ts: the phone's TipTap editor
// formats and Markdown mapping, plus a markdown-it reader) into one browser script
// for the desktop app's web view: assets/web/note-editor.js exposes `FiloNote`.
// It runs inside Filo's phone app repository, which isn't public, as
// `node apps/desktop/scripts/build-note-editor.mjs` from that repository's root; the bundle is committed here.
import { build } from 'esbuild';

await build({
  entryPoints: ['apps/desktop/web/note-editor.ts'],
  outfile: 'apps/desktop/assets/web/note-editor.js',
  bundle: true,
  format: 'iife',
  platform: 'browser',
  target: 'safari15',
  minify: true,
  legalComments: 'none',
  loader: { '.css': 'text', '.svg': 'text' },
  // The formats import React Native types only; keep any stray runtime import out.
  external: ['react-native', 'react', '@tiptap/react'],
});
console.log('wrote apps/desktop/assets/web/note-editor.js');
