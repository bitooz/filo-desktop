// Writes a made-up glance, as `glances:get` returns it, for the desktop's glance check:
//   node scripts/glance-fixture.mjs > /tmp/glance.json
//   FILO_CONFIG_DIR="$(mktemp -d)" FILO_WINDOW=1004x760 FILO_OPEN=glances:/tmp/glance.json cargo run
// The page comes from the same code the local backend uses (crates/filo-local/src/glance_document.rs, through
// `filo-local glance-document`), so the check sees what the app gets.
import { execFileSync } from 'node:child_process';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const glanceDocument = (glance) => execFileSync(
  'cargo', ['run', '--quiet', '-p', 'filo-local', '--bin', 'filo-local', '--', 'glance-document'],
  { cwd: root, input: JSON.stringify(glance), encoding: 'utf8', stdio: ['pipe', 'pipe', 'inherit'] },
);

const weeks = [
  ['Aug 10', 6, 16], ['Aug 17', 7, 15], ['Aug 24', 8, 14], ['Aug 31', 9, 12],
  ['Sep 7', 11, 11], ['Sep 14', 12, 10], ['Sep 21', 13, 9], ['Sep 28', 14, 8],
];
const max = 18;
const bars = weeks.map(([week, meetings, focus]) => {
  const tt = JSON.stringify({ t: `Week of ${week}`, rows: [['var(--c1)', 'Meetings', `${meetings} h`], ['var(--c2)', 'Focus', `${focus} h`]] });
  return `<div class="wk hit" tabindex="0" data-tt='${tt}'>
    <div class="pair"><i style="height:${(meetings / max) * 100}%;background:var(--c1)"></i><i style="height:${(focus / max) * 100}%;background:var(--c2)"></i></div>
    <span class="axis">${week.split(' ')[1]}</span></div>`;
}).join('');
const html = `<style>
.chart{position:relative;height:150px;display:flex;align-items:flex-end;gap:6%;padding:0 2px}
html[data-size=large] .chart{height:260px}
.wk{flex:1;height:100%;display:flex;flex-direction:column;align-items:center;gap:6px}
.pair{flex:1;width:100%;display:flex;align-items:flex-end;justify-content:center;gap:3px}
.pair i{display:block;width:40%;border-radius:4px 4px 0 0}
</style>
<div class="chart">${bars}</div>
<div class="legend-row"><span><i class="key-box" style="--k:var(--c1)"></i>Meetings</span><span><i class="key-box" style="--k:var(--c2)"></i>Focus</span><span class="grow"></span><span class="muted">Hours a week</span></div>`;

const glance = {
  title: 'Focus and meetings, last 8 weeks',
  asOf: 'Aug 10 to Oct 4',
  html,
  notes: 'Meetings more than doubled since August.\nFocus time halved over the same weeks.',
};
const text = weeks.map(([week, meetings, focus]) => `Week of ${week}: ${meetings} h meetings, ${focus} h focus.`).join('\n');

process.stdout.write(JSON.stringify({
  _id: 'fixture-glance',
  title: glance.title,
  asOf: glance.asOf,
  icon: 'chart-column-stacked',
  source: 'your calendar',
  visualLabel: 'Chart',
  text,
  notes: glance.notes,
  table: { columns: ['Week of', 'Meetings', 'Focus'], rows: weeks.map(([week, meetings, focus]) => [week, `${meetings} h`, `${focus} h`]) },
  createdAt: Date.now(),
  document: glanceDocument(glance),
}, null, 2));
