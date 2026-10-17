// Filo's browser: what agents' tools do inside a page (browser.rs). Runs as the body of an async function in a
// JavaScript world of Filo's own (WKContentWorld.defaultClientWorld): it shares the page's DOM, not its scripts. `a` is
// the request as JSON ({op, …}); it returns its answer as JSON, {error} when it failed. Refs ([e5]) are kept per page
// in this world, so an element keeps its ref while the page is open.
const S = globalThis.__filo || (globalThis.__filo = { next: 1, ids: new WeakMap(), els: new Map() });
const arg = JSON.parse(a);
const MAX = 40000;
const SKIP = new Set(['SCRIPT', 'STYLE', 'NOSCRIPT', 'TEMPLATE', 'HEAD', 'META', 'LINK', 'SVG', 'CANVAS', 'VIDEO', 'AUDIO', 'OBJECT']);
const ROLES = ['button', 'link', 'checkbox', 'radio', 'tab', 'menuitem', 'menuitemcheckbox', 'menuitemradio', 'option',
  'switch', 'combobox', 'textbox', 'searchbox', 'slider', 'spinbutton', 'treeitem'];

function clean(text, max = 120) {
  const t = String(text || '').replace(/\s+/g, ' ').trim();
  return t.length > max ? t.slice(0, max - 1) + '…' : t;
}

function refOf(el) {
  let id = S.ids.get(el);
  if (!id) {
    id = 'e' + S.next++;
    S.ids.set(el, id);
  }
  S.els.set(id, new WeakRef(el));
  return id;
}

function byRef(ref) {
  const id = String(ref || '').replace(/[\[\]\s]/g, '');
  const el = S.els.get(id)?.deref();
  if (!el || !el.isConnected) throw new Error(`There's no element ${id} on the page now: read the page again (browser_read) for its refs.`);
  return el;
}

function role(el) {
  const given = (el.getAttribute('role') || '').toLowerCase();
  if (ROLES.includes(given)) return given;
  const tag = el.tagName;
  if (tag === 'A' && el.hasAttribute('href')) return 'link';
  if (tag === 'BUTTON' || tag === 'SUMMARY') return 'button';
  if (tag === 'INPUT') {
    const type = (el.type || 'text').toLowerCase();
    if (type === 'hidden') return null;
    if (['button', 'submit', 'reset', 'image'].includes(type)) return 'button';
    if (type === 'checkbox' || type === 'radio') return type;
    if (type === 'range') return 'slider';
    if (type === 'file') return 'file';
    return 'textbox';
  }
  if (tag === 'TEXTAREA') return 'textbox';
  if (tag === 'SELECT') return 'select';
  if (el.isContentEditable && !(el.parentElement && el.parentElement.isContentEditable)) return 'textbox';
  return null;
}

// An element with a ref: one with a role, or an item the page lets be dragged (draggable="true": tiles, cards, list
// items), which browser_drag takes by ref. A draggable's line is a short name, and what's in it is read on.
function kindOf(el) {
  return role(el) || (el.getAttribute('draggable') === 'true' ? 'draggable' : null);
}

function nameOf(el) {
  const label = el.getAttribute('aria-label');
  if (clean(label)) return clean(label);
  const by = el.getAttribute('aria-labelledby');
  if (by) {
    const text = by.split(/\s+/).map((id) => el.ownerDocument.getElementById(id)?.innerText || '').join(' ');
    if (clean(text)) return clean(text);
  }
  if (el.labels && el.labels.length) {
    const text = [...el.labels].map((label) => label.innerText).join(' ');
    if (clean(text)) return clean(text);
  }
  if (el.tagName === 'INPUT' && ['button', 'submit', 'reset'].includes(el.type)) return clean(el.value || el.type);
  if (el.tagName === 'INPUT' || el.tagName === 'TEXTAREA' || el.tagName === 'SELECT') return clean(el.placeholder || el.title || el.name || '');
  const text = clean(el.innerText || el.textContent);
  if (text) return text;
  const img = el.querySelector && el.querySelector('img[alt]');
  if (img && clean(img.alt)) return clean(img.alt);
  return clean(el.title || el.getAttribute('alt') || '');
}

function fieldValue(el) {
  if (el.isContentEditable) return clean(el.innerText, 300);
  if (el.type === 'password') return '•'.repeat(Math.min((el.value || '').length, 12));
  return clean(el.value, 300);
}

function describe(el, kind) {
  kind = kind || kindOf(el) || el.tagName.toLowerCase();
  let line = `[${refOf(el)}] ${kind}`;
  const name = nameOf(el);
  if (name) line += ` "${name}"`;
  if (kind === 'textbox' || kind === 'searchbox' || kind === 'combobox' || kind === 'spinbutton') {
    line += ` value="${fieldValue(el)}"`;
    if (el.placeholder && clean(el.placeholder) !== name) line += ` placeholder="${clean(el.placeholder)}"`;
    if (el.tagName === 'INPUT' && el.type && el.type !== 'text') line += ` type=${el.type}`;
  }
  if (kind === 'checkbox' || kind === 'radio' || kind === 'switch' || kind === 'menuitemcheckbox') {
    const on = 'checked' in el && el.tagName === 'INPUT' ? el.checked : el.getAttribute('aria-checked') === 'true';
    line += on ? ' checked' : ' unchecked';
  }
  if (kind === 'select') {
    line += ` selected="${clean(el.selectedOptions?.[0]?.text)}" options: ` + [...el.options].slice(0, 30).map((option) => clean(option.text, 40)).join(' | ');
  }
  if (kind === 'select' && el.multiple) line += ' multiple';
  if (kind === 'file') {
    const names = [...(el.files || [])].map((file) => clean(file.name, 60));
    line += names.length ? ` chosen="${names.join(', ')}"` : ' (no file chosen)';
    if (el.accept) line += ` accept="${clean(el.accept, 60)}"`;
    if (el.multiple) line += ' multiple';
  }
  if (kind === 'slider') line += ` value=${el.value ?? el.getAttribute('aria-valuenow')}`;
  if (kind === 'link' && el.href && !String(el.href).startsWith('javascript:')) line += ` → ${clean(el.href, 160)}`;
  if (el.disabled || el.getAttribute('aria-disabled') === 'true') line += ' disabled';
  const expanded = el.getAttribute('aria-expanded');
  if (expanded) line += expanded === 'true' ? ' expanded' : ' collapsed';
  if (el.getAttribute('aria-selected') === 'true') line += ' selected';
  return line;
}

// The element's box in the top page's viewport, through the same-site frames it's in.
function boxInTop(el) {
  const r = el.getBoundingClientRect();
  let x = r.left, y = r.top;
  let view = el.ownerDocument.defaultView;
  while (view && view !== window) {
    let frame;
    try { frame = view.frameElement; } catch { break; }
    if (!frame) break;
    const fr = frame.getBoundingClientRect();
    x += fr.left + frame.clientLeft;
    y += fr.top + frame.clientTop;
    view = frame.ownerDocument.defaultView;
  }
  return { x, y, width: r.width, height: r.height };
}

function walk(root, lines, budget) {
  let text = '';
  const flush = (prefix = '') => {
    const line = clean(text, 2000);
    text = '';
    if (line) {
      lines.push(prefix + line);
      budget.size += line.length;
    }
  };
  const visit = (node) => {
    if (budget.size > MAX * 1.5) return;
    if (node.nodeType === 3) {
      text += node.textContent;
      return;
    }
    if (node.nodeType !== 1) return;
    const el = node;
    const tag = el.tagName.toUpperCase();
    if (SKIP.has(tag) || el.getAttribute('aria-hidden') === 'true') return;
    const style = getComputedStyle(el);
    if (style.display === 'none' || style.visibility === 'hidden') return;
    if (tag === 'IFRAME' || tag === 'FRAME') {
      flush();
      let doc = null;
      try { doc = el.contentDocument; } catch {}
      if (doc && doc.body) {
        lines.push(`[frame "${clean(el.title || el.name || '')}"]`);
        walk(doc.body, lines, budget);
        lines.push('[end of frame]');
      } else {
        const box = boxInTop(el);
        let from = '';
        try { from = new URL(el.src, location.href).host; } catch {}
        lines.push(`[frame from ${from || 'another site'}${el.title ? ` "${clean(el.title)}"` : ''} at x=${Math.round(box.x)} y=${Math.round(box.y)} ${Math.round(box.width)}×${Math.round(box.height)}: can't be read; use browser_screenshot and browser_click_at]`);
      }
      return;
    }
    const kind = kindOf(el);
    if (kind) {
      flush();
      const line = kind === 'draggable' ? `[${refOf(el)}] draggable "${clean(nameOf(el), 60)}"` : describe(el, kind);
      lines.push(line);
      budget.size += line.length;
      if (kind !== 'draggable') return;
    }
    if (tag === 'IMG') {
      if (clean(el.alt)) text += ` [image: ${clean(el.alt)}] `;
      return;
    }
    if (tag === 'BR') {
      flush();
      return;
    }
    const block = !style.display.startsWith('inline') && style.display !== 'contents';
    if (block) flush();
    if (el.shadowRoot) for (const child of el.shadowRoot.childNodes) visit(child);
    for (const child of el.childNodes) visit(child);
    const heading = /^H([1-6])$/.exec(tag);
    if (heading) flush('#'.repeat(+heading[1]) + ' ');
    else if (tag === 'LI') flush('- ');
    else if (block) flush();
  };
  visit(root);
  flush();
}

// The element with the keyboard, inside same-site frames and shadow roots.
function focused() {
  let el = document.activeElement;
  for (;;) {
    if (el && el.shadowRoot && el.shadowRoot.activeElement) { el = el.shadowRoot.activeElement; continue; }
    if (el && (el.tagName === 'IFRAME' || el.tagName === 'FRAME')) {
      let inner = null;
      try { inner = el.contentDocument?.activeElement; } catch {}
      if (inner) { el = inner; continue; }
    }
    return el;
  }
}

function focusLine() {
  const el = focused();
  if (!el || el === document.body || el === document.documentElement) return 'nothing';
  if (el.tagName === 'IFRAME') return 'a frame from another site';
  return describe(el);
}

function header() {
  const doc = document.documentElement;
  return [
    `${document.title || '(no title)'} — ${location.href}`,
    `Viewport ${innerWidth}×${innerHeight}, scrolled to ${Math.round(scrollX)},${Math.round(scrollY)} of ${doc.scrollWidth}×${doc.scrollHeight}. Focus: ${focusLine()}`,
  ];
}

// A login form's fields on the page: the password field shown, and the username field before it (or one the page
// marks as a username or email when the password comes on a later step).
function loginFields() {
  const shown = (el) => !el.disabled && !el.readOnly && el.getClientRects().length > 0 && getComputedStyle(el).visibility !== 'hidden';
  const pass = [...document.querySelectorAll('input[type=password]')].find((el) => shown(el) && el.autocomplete !== 'new-password') || null;
  const texts = [...document.querySelectorAll('input')].filter((el) => shown(el) && ['text', 'email', 'tel'].includes(el.type));
  let user = texts.find((el) => /username|email/.test(el.autocomplete || '')) || null;
  if (!user && pass) user = texts.filter((el) => el.compareDocumentPosition(pass) & Node.DOCUMENT_POSITION_FOLLOWING).pop() || null;
  if (!user && !pass) user = texts.find((el) => el.type === 'email' || /user|email|login/i.test(`${el.name} ${el.id}`)) || null;
  return { user, pass };
}

const ops = {
  read({ find }) {
    const lines = [];
    if (document.body) walk(document.body, lines, { size: 0 });
    let body = lines;
    if (find) {
      const needle = String(find).toLowerCase();
      body = lines.filter((line) => line.toLowerCase().includes(needle));
      if (!body.length) body = [`Nothing on the page has "${find}".`];
    }
    let text = body.join('\n');
    if (text.length > MAX) text = text.slice(0, MAX) + '\n… (the page goes on: give `find` for the part you want)';
    return { text: header().join('\n') + '\n\n' + text };
  },

  // Where to click an element for real: its middle, scrolled into view, and whether it's on top there.
  target({ ref }) {
    const el = byRef(ref);
    el.scrollIntoView({ block: 'center', inline: 'center', behavior: 'instant' });
    const box = boxInTop(el);
    const local = el.getBoundingClientRect();
    const lx = local.left + local.width / 2, ly = local.top + local.height / 2;
    const x = box.x + box.width / 2, y = box.y + box.height / 2;
    let hit = local.width > 0 && local.height > 0 && x >= 0 && y >= 0 && x < innerWidth && y < innerHeight;
    // A menu (<select>) clicked for real opens AppKit's menu, which would hold the app: browser_select picks in it.
    if (el.tagName === 'SELECT') return { x, y, hit: false, select: true, line: describe(el) };
    if (hit) {
      const top = el.ownerDocument.elementFromPoint(lx, ly);
      const label = top && top.closest && top.closest('label');
      hit = !!top && (top === el || el.contains(top) || (label && el.labels && [...el.labels].includes(label)));
    }
    return { x, y, hit, line: describe(el) };
  },

  click({ ref }) {
    const el = byRef(ref);
    el.scrollIntoView({ block: 'center', inline: 'center', behavior: 'instant' });
    if (typeof el.click === 'function') el.click();
    else el.dispatchEvent(new MouseEvent('click', { bubbles: true, cancelable: true, view: window }));
    return { line: describe(el) };
  },

  focus({ ref, clear }) {
    const el = byRef(ref);
    el.scrollIntoView({ block: 'center', inline: 'center', behavior: 'instant' });
    const kind = role(el);
    if (el.tagName === 'SELECT') return { kind: 'select', line: describe(el) };
    el.focus({ preventScroll: true });
    if (clear) {
      if (typeof el.select === 'function' && (el.tagName === 'INPUT' || el.tagName === 'TEXTAREA')) el.select();
      else if (el.isContentEditable) el.ownerDocument.getSelection().selectAllChildren(el);
    }
    return { kind: kind || 'other', focused: focused() === el, line: describe(el) };
  },

  value({ ref }) {
    const el = byRef(ref);
    return { value: el.isContentEditable ? el.innerText : String(el.value ?? '') };
  },

  // Sets a field's text through the page, for when real keys didn't reach it.
  fill({ ref, text, clear }) {
    const el = byRef(ref);
    if (el.isContentEditable) {
      el.focus();
      if (clear) el.ownerDocument.getSelection().selectAllChildren(el);
      el.ownerDocument.execCommand('insertText', false, text);
    } else {
      const proto = el.tagName === 'TEXTAREA' ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
      const set = Object.getOwnPropertyDescriptor(proto, 'value').set;
      set.call(el, clear ? text : String(el.value ?? '') + text);
      el.dispatchEvent(new Event('input', { bubbles: true }));
      el.dispatchEvent(new Event('change', { bubbles: true }));
    }
    return { line: describe(el) };
  },

  // Picks a menu's (<select>'s) option by its text or value; a list that takes several (`multiple`) gets all of
  // `texts`, the rest unpicked.
  choose({ ref, text, texts }) {
    const el = byRef(ref);
    if (el.tagName !== 'SELECT') return { native: false, line: describe(el) };
    const options = [...el.options];
    const pick = (wanted) => {
      const want = String(wanted).trim().toLowerCase();
      const option = options.find((o) => o.text.trim().toLowerCase() === want || o.value.toLowerCase() === want)
        || options.find((o) => o.text.toLowerCase().includes(want));
      if (!option) throw new Error(`No option "${wanted}" in ${describe(el)}`);
      if (option.disabled) throw new Error(`The option "${clean(option.text)}" is disabled.`);
      return option;
    };
    const wanted = texts && texts.length ? texts : [text];
    if (wanted.length > 1 && !el.multiple) throw new Error(`${describe(el)} takes one option: give one.`);
    const chosen = wanted.map(pick);
    el.focus({ preventScroll: true });
    if (el.multiple) for (const option of options) option.selected = chosen.includes(option);
    else el.value = chosen[0].value;
    el.dispatchEvent(new Event('input', { bubbles: true }));
    el.dispatchEvent(new Event('change', { bubbles: true }));
    return { native: true, line: describe(el) };
  },

  // A list's option shown on the page (a custom menu or combo box the page drew, open): the one whose text is `text`
  // (else has it), scrolled into view, and where to click it; or the options shown, when none is.
  option({ text }) {
    const shown = (el) => el.getClientRects().length > 0 && getComputedStyle(el).visibility !== 'hidden';
    const all = [...document.querySelectorAll('[role=option],[role=menuitem],[role=menuitemradio],[role=menuitemcheckbox],[role=treeitem],[role=listbox] li,[role=menu] li')].filter(shown);
    const want = String(text).trim().toLowerCase();
    const label = (el) => clean(el.getAttribute('aria-label') || el.innerText || el.textContent, 80);
    const el = all.find((o) => label(o).toLowerCase() === want) || all.find((o) => label(o).toLowerCase().includes(want));
    if (!el) return { options: all.slice(0, 40).map(label).filter(Boolean) };
    el.scrollIntoView({ block: 'nearest', inline: 'nearest', behavior: 'instant' });
    const box = boxInTop(el);
    const x = box.x + box.width / 2, y = box.y + box.height / 2;
    return { x, y, hit: x >= 0 && y >= 0 && x < innerWidth && y < innerHeight, ref: refOf(el), line: describe(el) };
  },

  // The file field for `ref`: the field itself, one inside it, in its form, or the page's only one (a drop area's
  // hidden field).
  file_input({ ref }) {
    const el = byRef(ref);
    const file = (it) => it && it.tagName === 'INPUT' && it.type === 'file';
    const inputs = [...document.querySelectorAll('input[type=file]')];
    const found = file(el) ? el
      : el.querySelector('input[type=file]')
      || (el.htmlFor && file(document.getElementById(el.htmlFor)) ? document.getElementById(el.htmlFor) : null)
      || (el.closest('form') && el.closest('form').querySelector('input[type=file]'))
      || (inputs.length === 1 ? inputs[0] : null);
    if (!found) return { ref: null, count: inputs.length };
    return { ref: refOf(found), multiple: found.multiple, accept: found.accept || '', line: describe(found, 'file') };
  },

  // Clicks a file field through the page (it may be hidden): WebKit then asks for the files.
  open_chooser({ ref }) {
    const el = byRef(ref);
    if (typeof el.showPicker === 'function') {
      try { el.showPicker(); return {}; } catch {}
    }
    el.click();
    return {};
  },

  line({ ref }) {
    return { line: describe(byRef(ref)) };
  },

  files({ ref }) {
    const el = byRef(ref);
    return { names: [...(el.files || [])].map((file) => `${file.name} (${file.size} bytes)`), line: describe(el, 'file') };
  },

  // Whether the page (and its same-site frames) shows `text` now, any case.
  has({ text }) {
    const want = String(text).toLowerCase();
    const seen = (doc) => {
      if (!doc || !doc.body) return false;
      if ((doc.body.innerText || '').toLowerCase().includes(want)) return true;
      if ([...doc.querySelectorAll('input,textarea')].some((el) => String(el.value || '').toLowerCase().includes(want))) return true;
      return [...doc.querySelectorAll('iframe,frame')].some((frame) => { try { return seen(frame.contentDocument); } catch { return false; } });
    };
    return { found: seen(document) };
  },

  // Waits until the page stops changing: no change to its DOM for half a second (at most `most` ms).
  async quiet({ most }) {
    const start = Date.now();
    let last = Date.now();
    const watch = new MutationObserver(() => { last = Date.now(); });
    watch.observe(document, { subtree: true, childList: true, attributes: true, characterData: true });
    while (Date.now() - last < 500 && Date.now() - start < (most || 5000)) await new Promise((done) => setTimeout(done, 100));
    watch.disconnect();
    return { quiet: Date.now() - last >= 500, waited: Date.now() - start };
  },

  // The whole page's size, and where it's scrolled to (a full-page screenshot).
  size() {
    const doc = document.documentElement;
    return { width: Math.max(doc.scrollWidth, innerWidth), height: Math.max(doc.scrollHeight, document.body ? document.body.scrollHeight : 0, innerHeight), x: scrollX, y: scrollY };
  },

  scroll_to({ x, y }) {
    window.scrollTo({ left: x || 0, top: y || 0, behavior: 'instant' });
    return { x: scrollX, y: scrollY };
  },

  scroll({ ref, x, y }) {
    if (ref) {
      byRef(ref).scrollIntoView({ block: 'center', inline: 'center', behavior: 'instant' });
    } else {
      const before = [scrollX, scrollY];
      window.scrollBy({ left: x || 0, top: y || 0, behavior: 'instant' });
      if (before[0] === scrollX && before[1] === scrollY) {
        // The page itself doesn't scroll: the box that does, under the middle of the view.
        let el = document.elementFromPoint(innerWidth / 2, innerHeight / 2);
        while (el && el !== document.body) {
          const style = getComputedStyle(el);
          if ((el.scrollHeight > el.clientHeight && /auto|scroll/.test(style.overflowY)) || (el.scrollWidth > el.clientWidth && /auto|scroll/.test(style.overflowX))) {
            el.scrollBy({ left: x || 0, top: y || 0, behavior: 'instant' });
            break;
          }
          el = el.parentElement;
        }
      }
    }
    return { text: header()[1] };
  },

  // A drag's two ends: an element's middle by ref, or a spot (x, y) as it is. With both by ref the page scrolls so
  // both show; a spot from a screenshot keeps the page where it is, so a ref with it must be in view already.
  drag_ends({ from, to }) {
    const both = !!(from.ref && to.ref);
    if (both) {
      byRef(from.ref).scrollIntoView({ block: 'center', inline: 'center', behavior: 'instant' });
      byRef(to.ref).scrollIntoView({ block: 'nearest', inline: 'nearest', behavior: 'instant' });
    }
    const end = (it) => {
      if (!it.ref) return { x: it.x, y: it.y, line: ops.at(it).line };
      const el = byRef(it.ref);
      const box = boxInTop(el);
      const x = box.x + box.width / 2, y = box.y + box.height / 2;
      if (!(box.width > 0 && box.height > 0 && x >= 0 && y >= 0 && x < innerWidth && y < innerHeight)) {
        throw new Error(both
          ? `${it.ref} and the other end don't fit in the view together: drag in parts, or scroll and drag by spots of a new screenshot.`
          : `${it.ref} isn't in the view: scroll to it (browser_scroll) and take a new screenshot for the other end's spot.`);
      }
      return { x, y, line: describe(el) };
    };
    return { from: end(from), to: end(to) };
  },

  // Watches the next drag for the page's drop (browser_drag): whether it got one, and took it (canceled it, as a page
  // that handles a drop does). WebKit tells the page's script nothing else of how a drag and drop ended.
  drag_watch() {
    if (!S.dragWatch) {
      S.dragWatch = { drop: null };
      addEventListener('drop', (e) => { S.dragWatch.drop = e; }, true);
    }
    S.dragWatch.drop = null;
    return {};
  },

  drag_seen() {
    const drop = S.dragWatch && S.dragWatch.drop;
    return { dropped: !!drop, taken: !!(drop && drop.defaultPrevented) };
  },

  // What's at a spot of the viewport.
  at({ x, y }) {
    const el = document.elementFromPoint(x, y);
    if (!el) return { line: 'nothing (outside the page)' };
    if (el.tagName === 'IFRAME') {
      let from = '';
      try { from = new URL(el.src, location.href).host; } catch {}
      return { line: `a frame from ${from || 'another site'}` };
    }
    let target = el;
    while (target && !kindOf(target)) target = target.parentElement;
    const select = !!(el.closest && el.closest('select'));
    return { select, line: target ? describe(target) : `${el.tagName.toLowerCase()} "${clean(el.innerText || el.textContent, 80)}"` };
  },

  // Whether the page asks for a login (the browser window then offers a saved one).
  login_form() {
    const { user, pass } = loginFields();
    return { password: !!pass, username: !!user };
  },

  // Fills a saved login the user chose into the login form, only while the page is still on its site.
  fill_login({ host, username, password }) {
    const bare = (name) => String(name || '').toLowerCase().replace(/^www\./, '');
    if (bare(location.hostname) !== bare(host)) throw new Error('The page is on another site now.');
    const { user, pass } = loginFields();
    const set = (el, value) => {
      el.focus();
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set.call(el, value);
      el.dispatchEvent(new Event('input', { bubbles: true }));
      el.dispatchEvent(new Event('change', { bubbles: true }));
    };
    if (user && username) set(user, username);
    if (pass) set(pass, password);
    return { user: !!user, pass: !!pass };
  },

  // The middle of an element by id or CSS selector, scrolled into view (the browser tools check).
  box_of({ id, selector }) {
    const el = id ? document.getElementById(id) : document.querySelector(selector);
    if (!el) throw new Error(`Nothing is ${id || selector}`);
    el.scrollIntoView({ block: 'center', inline: 'center', behavior: 'instant' });
    const box = boxInTop(el);
    return { x: box.x + box.width / 2, y: box.y + box.height / 2 };
  },

  focus_line() {
    const el = focused();
    return { line: focusLine(), select: !!(el && el.tagName === 'SELECT') };
  },
};

try {
  const op = ops[arg.op];
  if (!op) throw new Error(`Unknown page op ${arg.op}`);
  return JSON.stringify(await op(arg) ?? null);
} catch (error) {
  return JSON.stringify({ error: String(error && error.message || error) });
}
