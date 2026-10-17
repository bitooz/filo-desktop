// Trusted host UI, outside the opaque-origin artifact iframe. Opening the menu
// never changes the iframe, its size, its document or its scroll position.
// Preview and Source, then the artifact's versions (newest first, the one shown
// checked, the current one marked, with when each was made; a long history
// scrolls inside), and with an older one shown, Show current version and
// Restore. The app sends what to show (`filoArtifactMenu`) and hears the choice.
(() => {
  const layer = document.createElement('div');
  layer.id = 'artifact-menu-layer';
  layer.hidden = true;
  layer.innerHTML = `<style>
    #artifact-menu-layer[hidden]{display:none}
    #artifact-menu-layer{position:fixed;inset:0;z-index:10}
    #artifact-host-menu{position:absolute;top:4px;right:110px;width:260px;max-height:calc(100% - 12px);box-sizing:border-box;padding:6px;
      display:flex;flex-direction:column;
      border:1px solid var(--border,#555);border-radius:12px;background:var(--raised,#333);color:var(--text,#eee);
      box-shadow:0 8px 24px #0004;font:13px system-ui,-apple-system,sans-serif}
    #artifact-host-menu .heading{flex:none;padding:6px 8px 4px;color:var(--textTertiary,#999);font-size:11px}
    #artifact-host-menu .rule{flex:none;border-top:1px solid var(--border,#555);margin:6px 0}
    #artifact-host-menu button{display:flex;align-items:center;gap:8px;flex:none;width:100%;min-height:30px;padding:0 8px;
      border:0;border-radius:4px;background:transparent;color:inherit;font:inherit;text-align:left;cursor:pointer}
    #artifact-host-menu button:hover,#artifact-host-menu button:focus-visible{background:var(--surfaceStrong,#444);outline:none}
    #artifact-host-menu button:disabled{color:var(--textTertiary,#999);cursor:default;background:transparent}
    #artifact-host-menu svg{width:15px;height:15px;flex:none;fill:none;stroke:currentColor;stroke-width:1.8;color:var(--textSecondary,#aaa)}
    #artifact-host-menu .check{margin-left:auto;padding-left:8px;color:var(--accent,#87bfff)}
    #artifact-host-menu .versions{flex:0 1 auto;min-height:0;max-height:264px;overflow-y:auto;overscroll-behavior:contain}
    #artifact-host-menu .version{min-height:42px;padding:4px 8px}
    #artifact-host-menu .version .lines{display:flex;flex-direction:column;gap:2px;min-width:0}
    #artifact-host-menu .version .name{display:flex;align-items:center;gap:6px}
    #artifact-host-menu .version .when{font-size:11px;color:var(--textSecondary,#aaa)}
    #artifact-host-menu .chip{padding:1px 6px;border-radius:999px;background:var(--successSoft,#2a4);color:var(--onSuccessSoft,#dfd);font-size:10px;font-weight:600}
    #artifact-host-menu .note{padding:6px 8px;color:var(--textTertiary,#999);font-size:11px}
    #artifact-host-menu .older[hidden]{display:none}
  </style><div id="artifact-host-menu" role="menu" aria-label="Artifact display">
    <button role="menuitemradio" data-mode="preview"><svg viewBox="0 0 24 24"><path d="M2 12s3-7 10-7 10 7 10 7-3 7-10 7S2 12 2 12Z"/><circle cx="12" cy="12" r="3"/></svg>Preview<span class="check">✓</span></button>
    <button role="menuitemradio" data-mode="source"><svg viewBox="0 0 24 24"><path d="m8 6-6 6 6 6m8-12 6 6-6 6m-3-15-2 18"/></svg>Source</button>
    <div class="rule"></div>
    <div class="heading">Versions</div>
    <div class="versions" role="group" aria-label="Versions"></div>
    <div class="older" hidden>
      <div class="rule"></div>
      <button role="menuitem" data-mode="current"><svg viewBox="0 0 24 24"><path d="M3 11a9 9 0 1 1 2.6 7M3 4v7h7"/></svg>Show current version</button>
      <button role="menuitem" data-mode="restore"><svg viewBox="0 0 24 24"><path d="M3 11a9 9 0 1 1 2.6 7M3 4v7h7m2-4v5l3 2"/></svg><span class="restore-label">Restore this version…</span></button>
    </div>
  </div>`;
  document.body.appendChild(layer);
  const menu = layer.querySelector('[role=menu]');
  const list = menu.querySelector('.versions');
  const send = message => {
    layer.hidden = true;
    if (window.filoHostPost) window.filoHostPost(Object.assign({host: 'menu'}, message));
  };
  layer.addEventListener('pointerdown', event => { if (event.target === layer) send({mode: 'close'}); });
  menu.addEventListener('click', event => {
    const button = event.target.closest('button');
    if (!button || button.disabled) return;
    if (button.dataset.version) send({mode: 'version', version: Number(button.dataset.version)});
    else if (button.dataset.mode) send({mode: button.dataset.mode});
  });
  // A focused version row comes into view inside the list (the page itself never scrolls).
  const reveal = button => {
    if (button.parentElement !== list) return;
    const top = button.offsetTop - list.offsetTop;
    if (top < list.scrollTop) list.scrollTop = top;
    else if (top + button.offsetHeight > list.scrollTop + list.clientHeight) list.scrollTop = top + button.offsetHeight - list.clientHeight;
  };
  layer.addEventListener('keydown', event => {
    const buttons = [...menu.querySelectorAll('button')].filter(button => !button.closest('[hidden]') && !button.disabled);
    let index = buttons.indexOf(document.activeElement);
    if (event.key === 'Escape' || event.key === 'Tab') { event.preventDefault(); send({mode: 'close'}); }
    else if (['ArrowDown', 'ArrowUp', 'Home', 'End'].includes(event.key)) {
      event.preventDefault();
      index = event.key === 'Home' ? 0 : event.key === 'End' ? buttons.length - 1 :
        (index + (event.key === 'ArrowDown' ? 1 : -1) + buttons.length) % buttons.length;
      buttons[index].focus({preventScroll: true});
      reveal(buttons[index]);
    }
  });
  const element = (tag, className, text) => {
    const node = document.createElement(tag);
    node.className = className;
    if (text !== undefined) node.textContent = text;
    return node;
  };
  // `state`: {open, right, shown, older, versions: [{version, when, current}] | null, error, more, restoring}.
  window.filoArtifactMenu = state => {
    const changed = layer.hidden === state.open;
    layer.hidden = !state.open;
    menu.style.right = `${Math.max(8, state.right)}px`;
    menu.querySelectorAll('[data-mode]').forEach(button => button.setAttribute('aria-checked', String(button.dataset.mode === 'preview')));
    list.textContent = '';
    if (!state.versions) {
      list.append(element('div', 'note', state.error || 'Loading versions…'));
    } else {
      for (const row of state.versions) {
        const button = element('button', 'version');
        button.setAttribute('role', 'menuitemradio');
        button.dataset.version = row.version;
        button.setAttribute('aria-checked', String(row.version === state.shown));
        const lines = element('div', 'lines');
        const name = element('div', 'name', `Version ${row.version}`);
        if (row.current) name.append(element('span', 'chip', 'Current'));
        lines.append(name, element('div', 'when', row.when));
        button.append(lines);
        if (row.version === state.shown) button.append(element('span', 'check', '✓'));
        list.append(button);
      }
      if (state.more) list.append(element('div', 'note', `Showing the latest ${state.versions.length} versions.`));
    }
    menu.querySelector('.older').hidden = !state.older;
    const restore = menu.querySelector('[data-mode=restore]');
    restore.disabled = Boolean(state.restoring);
    restore.querySelector('.restore-label').textContent = state.restoring ? 'Restoring…' : 'Restore this version…';
    if (state.open && changed) menu.querySelector('button').focus({preventScroll: true});
    const checked = list.querySelector('[aria-checked=true]');
    if (state.open && checked) reveal(checked);
  };
})();
