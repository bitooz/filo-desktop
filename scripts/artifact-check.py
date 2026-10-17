#!/usr/bin/env python3
"""Check artifact loading and menu/scroll preservation on a disposable copy.

Build first, then run:
  python3 scripts/artifact-check.py --artifact-id ID [--artifact-id OTHER ...] --output-dir target/artifact-proof
The first ID is opened, checked and timed; two more are hovered and opened too.
--record also records the window at 60 fps (scripts/flash-recorder.swift) to catch a white frame before a page draws.
The running app and source database are left alone.
"""
import argparse
import json
import os
from pathlib import Path
import re
import sqlite3
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parent.parent
PROBE = """<script>const probeToken=String(Math.random());
addEventListener('message',e=>{if(!e.data?.artifactProbe)return;
if(e.data.artifactProbe==='scroll')scrollTo(0,e.data.y);
requestAnimationFrame(()=>parent.postMessage({artifactProbe:{y:scrollY,token:probeToken}},'*'))});</script>"""


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--artifact-id', required=True, action='append')
    parser.add_argument('--source-root', type=Path, default=Path.home() / '.config/filo/local')
    parser.add_argument('--binary', type=Path, default=ROOT / 'target/debug/filo')
    parser.add_argument('--output-dir', type=Path, default=ROOT / 'target/artifact-proof')
    parser.add_argument('--prepare-ms', type=int, default=2000, help='time on Artifacts list before first click; 0 tests a cold direct open')
    parser.add_argument('--theme', choices=['dark', 'light'], default='dark')
    parser.add_argument('--record', action='store_true', help='record the window and report white frames (only where Screen Recording is already allowed; it never asks)')
    args = parser.parse_args()
    if args.prepare_ms < 0:
        parser.error('prepare-ms must be nonnegative')
    if not all(re.fullmatch(r'[A-Za-z0-9_-]+', id) for id in args.artifact_id):
        parser.error('invalid artifact id')
    output = args.output_dir.resolve()
    output.mkdir(parents=True, exist_ok=True)
    (ROOT / 'target').mkdir(exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='artifact-smoke-', dir=ROOT / 'target') as temporary:
        config = Path(temporary)
        local = config / 'local'
        local.mkdir()
        # Copy only artifact rows/files, never chats, credentials, scheduled work or coding tasks.
        with sqlite3.connect(f'file:{args.source_root.resolve()}/filo.db?mode=ro', uri=True) as source:
            with sqlite3.connect(local / 'filo.db') as fixture:
                rust = (ROOT / 'crates/filo-local/src/artifacts.rs').read_text()
                schema = rust.split('pub const SCHEMA: &str = "', 1)[1].split('";', 1)[0]
                fixture.executescript(schema)
                for id in args.artifact_id:
                    for table, key in [('artifacts', 'id'), ('artifact_versions', 'artifact_id')]:
                        columns = [row[1] for row in fixture.execute(f'PRAGMA table_info({table})')]
                        rows = source.execute(f'SELECT {",".join(columns)} FROM {table} WHERE {key}=?', (id,)).fetchall()
                        if not rows:
                            raise RuntimeError(f'No {table} rows for {id}')
                        fixture.executemany(f'INSERT INTO {table} VALUES ({",".join("?" for _ in columns)})', rows)
                # An unopened fixture proves speculative preparation leaves its status alone.
                fixture.execute('UPDATE artifacts SET opened_at=NULL WHERE id=?', (args.artifact_id[0],))
                files = fixture.execute('SELECT artifact_id, file FROM artifact_versions').fetchall()
        for id, name in files:
            if not re.fullmatch(r'v[0-9]+\.html', name):
                raise RuntimeError(f'Unexpected artifact file: {name}')
            folder = local / 'artifacts' / id
            folder.mkdir(parents=True, exist_ok=True)
            document = (args.source_root / 'artifacts' / id / name).read_text()
            (folder / name).write_text(document + PROBE)
        (local / 'profile.json').write_text(json.dumps({'userName': 'Artifact check', 'agentName': 'Filo'}))
        (config / 'desktop-welcome.json').write_text('{"seen":true}')
        (config / 'desktop.json').write_text(json.dumps({'themeMode': args.theme}))
        env = dict(os.environ, FILO_CONFIG_DIR=str(config), FILO_NO_LAUNCH_AGENT='1',
                   FILO_SMOKE='artifacts', FILO_ARTIFACT_ID=args.artifact_id[0], FILO_ARTIFACT_PREPARE_MS=str(args.prepare_ms), FILO_WINDOW='1100x760', FILO_DRIVE_SHOTS=str(output))
        # WebKit throttles animation frames in inactive windows. Measure a focused window,
        # as when the user opens an artifact, rather than the background floating test mode.
        env.pop('FILO_WINDOW_FLOAT', None)
        env.pop('FILO_OPEN', None)
        recorder = None
        if args.record:
            recorder_binary = ROOT / 'target/flash-recorder'
            subprocess.run(['swiftc', '-O', str(ROOT / 'scripts/flash-recorder.swift'), '-o', str(recorder_binary)], check=True)
            env['FILO_ARTIFACT_WAIT_FILE'] = str(config / 'recording')
        with (output / 'artifacts-check.log').open('w') as log:
            app = subprocess.Popen([str(args.binary.resolve())], cwd=ROOT, env=env, stdout=log, stderr=subprocess.STDOUT)
            if args.record:
                recorder = subprocess.Popen([str(recorder_binary), str(app.pid), '1100', str(output), '60'], stdout=subprocess.PIPE, text=True)
                # The check starts once the recorder sees the window.
                print(recorder.stdout.readline().strip())
                (config / 'recording').touch()
            try:
                code = app.wait(timeout=60)
            finally:
                if app.poll() is None:
                    app.kill()
        print((output / 'artifacts-check.log').read_text())
        if recorder:
            print(recorder.communicate(timeout=30)[0].strip())
        if code:
            raise SystemExit(f'check failed ({code})')


if __name__ == '__main__':
    main()
