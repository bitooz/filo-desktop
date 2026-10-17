// The desktop note page, run in the app's native web view (src/screens/note.rs).
// Edit mode is the phone's editor: react-native-enriched-html's TipTap formats and
// HTML normalizers (its web build), fed through the phone's own Markdown ⇄ HTML
// mapping (src/features/filo/note-rich-text.ts), so both apps read and write the
// same Markdown. A note that mapping can't hold stays plain text, as on the phone.
// Read mode renders the full Markdown with markdown-it. The skill editor
// (src/screens/skill_editor.rs) shows the same page with `fields`: the title is
// the skill's name, a "when to use it" line sits under it, and both stay one line.
// A skill's body is edited as its Markdown source, never through the phone's
// mapping, so a save changes only what was typed.
// It's built with scripts/build-note-editor.mjs inside Filo's phone app repository,
// which isn't public (the imports below reach into its code and node_modules), so
// it can't be rebuilt from this one: the bundle, assets/web/note-editor.js, is
// committed. THIRD_PARTY_NOTICES.md lists the packages in it.

import { Editor } from '@tiptap/core';
import Document from '@tiptap/extension-document';
import History from '@tiptap/extension-history';
import Paragraph from '@tiptap/extension-paragraph';
import Text from '@tiptap/extension-text';
import { Placeholder } from '@tiptap/extensions/placeholder';
import { Selection } from '@tiptap/pm/state';
import MarkdownIt from 'markdown-it';

import { markdownToNoteHtml, noteHtmlToMarkdown } from '../../../src/features/filo/note-rich-text';
import { DEFAULT_HTML_STYLE } from '../../../node_modules/react-native-enriched-html/src/utils/defaultHtmlStyle';
import editorCss from '../../../node_modules/react-native-enriched-html/src/web/EnrichedText.css';
import { EnrichedBlockquote } from '../../../node_modules/react-native-enriched-html/src/web/formats/EnrichedBlockquote';
import { EnrichedBold } from '../../../node_modules/react-native-enriched-html/src/web/formats/EnrichedBold';
import { EnrichedCheckboxItem } from '../../../node_modules/react-native-enriched-html/src/web/formats/EnrichedCheckboxItem';
import { EnrichedCheckboxList } from '../../../node_modules/react-native-enriched-html/src/web/formats/EnrichedCheckboxList';
import { EnrichedCode } from '../../../node_modules/react-native-enriched-html/src/web/formats/EnrichedCode';
import { EnrichedCodeBlock } from '../../../node_modules/react-native-enriched-html/src/web/formats/EnrichedCodeBlock';
import { EnrichedHeading } from '../../../node_modules/react-native-enriched-html/src/web/formats/EnrichedHeading';
import { EnrichedItalic } from '../../../node_modules/react-native-enriched-html/src/web/formats/EnrichedItalic';
import { EnrichedLink } from '../../../node_modules/react-native-enriched-html/src/web/formats/EnrichedLink';
import { EnrichedListItem } from '../../../node_modules/react-native-enriched-html/src/web/formats/EnrichedListItem';
import { EnrichedOrderedList } from '../../../node_modules/react-native-enriched-html/src/web/formats/EnrichedOrderedList';
import { EnrichedStrike } from '../../../node_modules/react-native-enriched-html/src/web/formats/EnrichedStrike';
import { EnrichedUnorderedList } from '../../../node_modules/react-native-enriched-html/src/web/formats/EnrichedUnorderedList';
import { normalizeHtmlFromTiptap, prepareHtmlForTiptap } from '../../../node_modules/react-native-enriched-html/src/web/normalization/tiptapHtmlNormalizer';
import { MergeAdjacentSameKindBlocksPlugin } from '../../../node_modules/react-native-enriched-html/src/web/pmPlugins/MergeAdjacentSameKindBlocksPlugin';
import { ShortcutPlugin } from '../../../node_modules/react-native-enriched-html/src/web/pmPlugins/ShortcutPlugin';
import { StrictMarksPlugin } from '../../../node_modules/react-native-enriched-html/src/web/pmPlugins/StrictMarksPlugin';
import { StripBoldInStyledHeadingsPlugin } from '../../../node_modules/react-native-enriched-html/src/web/pmPlugins/StripBoldInStyledHeadingsPlugin';
import { StripMarksInCodeBlockPlugin } from '../../../node_modules/react-native-enriched-html/src/web/pmPlugins/StripMarksInCodeBlockPlugin';
import { TextShortcutsPlugin } from '../../../node_modules/react-native-enriched-html/src/web/pmPlugins/TextShortcutsPlugin';

import boldIcon from '../assets/icons/bold.svg';
import headingIcon from '../assets/icons/heading.svg';
import italicIcon from '../assets/icons/italic.svg';
import linkIcon from '../assets/icons/link.svg';
import listIcon from '../assets/icons/list.svg';
import orderedIcon from '../assets/icons/list-ordered.svg';
import taskIcon from '../assets/icons/list-todo.svg';
import strikeIcon from '../assets/icons/strikethrough.svg';

type Mode = 'read' | 'edit';
/** A skill's page: placeholders, a fixed name, and the line under the title. */
type Fields = {
  titlePlaceholder: string;
  titleReadOnly: boolean;
  description: string;
  descriptionPlaceholder: string;
  descriptionMax: number;
  bodyPlaceholder: string;
};
type Show = { mode: Mode; title: string; markdown: string; focus?: 'title' | 'description' | 'body'; fields?: Fields };
type Message =
  | { type: 'title'; title: string }
  | { type: 'description'; description: string }
  | { type: 'save' }
  | { type: 'body'; markdown: string }
  | { type: 'blur' }
  | { type: 'edit' }
  | { type: 'task'; line: number }
  | { type: 'link'; url: string }
  | { type: 'ready' };

declare global {
  interface Window {
    ipc: { postMessage(message: string): void };
    FiloNote: typeof api;
  }
}

const post = (message: Message) => window.ipc.postMessage(JSON.stringify(message));

// The phone's heading sizes (rich-note-input.tsx noteHtmlStyle), all bold.
const HTML_STYLE = {
  ...DEFAULT_HTML_STYLE,
  h1: { fontSize: 30, bold: true },
  h2: { fontSize: 26, bold: true },
  h3: { fontSize: 22, bold: true },
  h4: { fontSize: 20, bold: true },
  h5: { fontSize: 17, bold: true },
  h6: { fontSize: 16, bold: true },
};
// rich-note-input.tsx TEXT_SHORTCUTS.
const TEXT_SHORTCUTS = [
  { trigger: '- ', style: 'unordered_list' },
  { trigger: '* ', style: 'unordered_list' },
  { trigger: '1. ', style: 'ordered_list' },
  { trigger: '[] ', style: 'checkbox_list' },
  { trigger: '> ', style: 'blockquote' },
] as const;

const style = document.createElement('style');
style.textContent = editorCss;
document.head.appendChild(style);

const root = document.getElementById('note') as HTMLElement;
const titleField = document.getElementById('title') as HTMLTextAreaElement;
const titleView = document.getElementById('title-view') as HTMLElement;
const descriptionField = document.getElementById('description') as HTMLTextAreaElement;
const bodyHost = document.getElementById('body') as HTMLElement;
const toolbar = document.getElementById('toolbar') as HTMLElement;

let mode: Mode = 'read';
let editor: Editor | null = null;
let plain: HTMLTextAreaElement | null = null;
let lastMarkdown = '';
let mouseDown = false;
let fields: Fields | undefined;
const BODY_PLACEHOLDER = 'Start writing…';

// --- title ---

function sizeTextarea(field: HTMLTextAreaElement) {
  field.style.height = 'auto';
  field.style.height = `${field.scrollHeight}px`;
}

/** A skill's name and line take no line breaks: pasted ones become spaces, where the cursor was. */
function oneLine(field: HTMLTextAreaElement) {
  if (!fields || !/[\r\n]/.test(field.value)) return;
  const at = field.selectionStart;
  field.value = field.value.replace(/[\r\n]/g, ' ');
  field.setSelectionRange(at, at);
}

/** Puts the caret at the body's start now, not a frame later as TipTap's `focus('start')` does: keys typed right
 *  after Return land in the body. */
function focusBody() {
  if (editor) {
    const { state, view } = editor;
    view.dispatch(state.tr.setSelection(Selection.atStart(state.doc)));
    view.focus();
  } else plain?.focus();
}

titleField.addEventListener('input', () => {
  oneLine(titleField);
  sizeTextarea(titleField);
  post({ type: 'title', title: titleField.value });
});
titleField.addEventListener('keydown', (event) => {
  // Enter goes on to the body (a skill's line first), as on the phone.
  if (event.key === 'Enter') {
    event.preventDefault();
    if (fields) descriptionField.focus();
    else focusBody();
  }
});
descriptionField.addEventListener('input', () => {
  oneLine(descriptionField);
  sizeTextarea(descriptionField);
  post({ type: 'description', description: descriptionField.value });
});
descriptionField.addEventListener('keydown', (event) => {
  if (event.key === 'Enter') {
    event.preventDefault();
    focusBody();
  }
});
// ⌘S saves (the skill editor; the note page saves on its own).
document.addEventListener('keydown', (event) => {
  if (event.metaKey && !event.shiftKey && !event.altKey && event.key.toLowerCase() === 's') {
    event.preventDefault();
    post({ type: 'save' });
  }
});

// --- read mode ---

const reader = new MarkdownIt('default', { html: false, linkify: true, breaks: false });
reader.linkify.set({ fuzzyEmail: false, fuzzyLink: false });
const TASK = /^\[([ xX])\](?:\s+|$)/;
// Task items get a checkbox that toggles their source line (`toggle_task_line` in note_text.rs).
reader.core.ruler.after('inline', 'filo-tasks', (state) => {
  const tokens = state.tokens;
  for (let i = 0; i < tokens.length - 2; i++) {
    if (tokens[i].type !== 'list_item_open' || tokens[i + 2].type !== 'inline') continue;
    const inline = tokens[i + 2];
    const match = TASK.exec(inline.content);
    const first = inline.children?.[0];
    if (!match || !first || first.type !== 'text' || !TASK.test(first.content)) continue;
    const line = tokens[i].map?.[0] ?? -1;
    first.content = first.content.replace(TASK, '');
    const box = new state.Token('html_inline', '', 0);
    const checked = match[1] !== ' ';
    box.content = `<span class="task-box${checked ? ' checked' : ''}" data-line="${line}" role="checkbox" aria-checked="${checked}"></span>`;
    inline.children!.unshift(box);
    tokens[i].attrJoin('class', `task-item${checked ? ' done' : ''}`);
  }
});

function renderReader(markdown: string) {
  if (!markdown.trim()) {
    bodyHost.innerHTML = '<p class="empty">Nothing here yet. Double-click to start writing.</p>';
    return;
  }
  bodyHost.innerHTML = reader.render(markdown);
}

root.addEventListener('click', (event) => {
  const target = event.target as HTMLElement;
  const box = target.closest('.task-box') as HTMLElement | null;
  if (box && mode === 'read') {
    post({ type: 'task', line: Number(box.dataset.line) });
    return;
  }
  const link = target.closest('a') as HTMLAnchorElement | null;
  if (link) {
    event.preventDefault();
    // In the editor a link is text to edit; ⌘-click opens it.
    if (mode === 'read' || event.metaKey) post({ type: 'link', url: link.getAttribute('href') ?? '' });
  }
});
root.addEventListener('dblclick', (event) => {
  if (mode === 'read' && !(event.target as HTMLElement).closest('a, .task-box')) post({ type: 'edit' });
});

// --- edit mode ---

function bodyMarkdown(): string {
  if (plain) return plain.value;
  if (!editor) return lastMarkdown;
  return noteHtmlToMarkdown(normalizeHtmlFromTiptap(editor.getHTML(), () => undefined));
}

function bodyChanged() {
  const markdown = bodyMarkdown();
  if (markdown === lastMarkdown) return;
  lastMarkdown = markdown;
  post({ type: 'body', markdown });
}

function createEditor(html: string) {
  const host = document.createElement('div');
  host.className = 'eti-editor';
  bodyHost.appendChild(host);
  editor = new Editor({
    element: host,
    extensions: [
      Document,
      Paragraph,
      Text,
      History,
      EnrichedBold,
      EnrichedItalic,
      EnrichedStrike,
      EnrichedCode,
      EnrichedLink.configure({ getLinkRegex: () => undefined }),
      EnrichedHeading,
      EnrichedBlockquote,
      EnrichedCodeBlock,
      EnrichedListItem,
      EnrichedCheckboxItem,
      EnrichedUnorderedList,
      EnrichedOrderedList,
      EnrichedCheckboxList,
      StripMarksInCodeBlockPlugin,
      StripBoldInStyledHeadingsPlugin.configure({ getHtmlStyle: () => HTML_STYLE }),
      MergeAdjacentSameKindBlocksPlugin,
      StrictMarksPlugin,
      ShortcutPlugin.configure({ getHtmlStyle: () => HTML_STYLE }),
      TextShortcutsPlugin.configure({ getTextShortcuts: () => TEXT_SHORTCUTS, getHtmlStyle: () => HTML_STYLE }),
      Placeholder.configure({ placeholder: fields?.bodyPlaceholder ?? BODY_PLACEHOLDER, showOnlyWhenEditable: true }),
    ],
    onCreate: ({ editor: created }) => {
      // As the phone sets it: after creation, so the formats' plugins run on it.
      created.commands.setContent(prepareHtmlForTiptap(html, false), { emitUpdate: false });
    },
    onUpdate: bodyChanged,
    onSelectionUpdate: () => placeToolbar(),
    onFocus: () => placeToolbar(),
  });
}

function createPlain(markdown: string) {
  plain = document.createElement('textarea');
  plain.className = 'plain';
  plain.placeholder = fields?.bodyPlaceholder ?? BODY_PLACEHOLDER;
  plain.value = markdown;
  bodyHost.appendChild(plain);
  sizeTextarea(plain);
  plain.addEventListener('input', () => {
    sizeTextarea(plain!);
    bodyChanged();
  });
}

function teardown() {
  editor?.destroy();
  editor = null;
  plain = null;
  bodyHost.innerHTML = '';
  hideToolbar();
}

// --- format toolbar ---

type Action = 'bold' | 'italic' | 'strike' | 'link' | 'heading' | 'bullet' | 'number' | 'task';
const ACTIONS: [Action, string, string][] = [
  ['bold', 'Bold', boldIcon],
  ['italic', 'Italic', italicIcon],
  ['strike', 'Strikethrough', strikeIcon],
  ['link', 'Link', linkIcon],
  ['heading', 'Heading', headingIcon],
  ['bullet', 'Bullet list', listIcon],
  ['number', 'Numbered list', orderedIcon],
  ['task', 'Task list', taskIcon],
];

const buttons = document.createElement('div');
buttons.className = 'buttons';
ACTIONS.forEach(([action, label, icon], index) => {
  // Groups: bold/italic/strike | link | heading and lists.
  if (index === 3 || index === 4) buttons.appendChild(Object.assign(document.createElement('span'), { className: 'sep' }));
  const button = document.createElement('button');
  button.type = 'button';
  button.title = label;
  button.dataset.action = action;
  button.innerHTML = icon;
  buttons.appendChild(button);
});
const linkForm = document.createElement('form');
linkForm.className = 'link-form';
const linkInput = Object.assign(document.createElement('input'), { type: 'url', placeholder: 'Paste or type a link' });
linkForm.appendChild(linkInput);
toolbar.append(buttons, linkForm);

// Clicks on the toolbar keep the editor's selection.
toolbar.addEventListener('mousedown', (event) => {
  if (event.target !== linkInput) event.preventDefault();
});
buttons.addEventListener('click', (event) => {
  const action = (event.target as HTMLElement).closest('button')?.dataset.action as Action | undefined;
  if (action) format(action);
});

function format(action: Action) {
  if (!editor) return;
  const chain = editor.chain().focus();
  if (action === 'bold') chain.toggleBold().run();
  else if (action === 'italic') chain.toggleItalic().run();
  else if (action === 'strike') chain.toggleStrike().run();
  else if (action === 'bullet') chain.toggleUnorderedList().run();
  else if (action === 'number') chain.toggleOrderedList().run();
  else if (action === 'task') chain.toggleCheckboxList(false).run();
  else if (action === 'heading') {
    // Keeps the heading's level, else h2 (rich-note-input.tsx).
    const level = ([1, 2, 3, 4, 5, 6] as const).find((n) => editor!.isActive('heading', { level: n })) ?? 2;
    chain.toggleHeading({ level }).run();
  } else if (action === 'link') {
    linkInput.value = (editor.getAttributes('link').href as string | undefined) ?? '';
    toolbar.classList.add('linking');
    linkInput.focus();
    return;
  }
  placeToolbar();
}

linkForm.addEventListener('submit', (event) => {
  event.preventDefault();
  if (!editor) return;
  const href = linkInput.value.trim();
  const chain = editor.chain().focus().extendMarkRange('link');
  if (href) chain.setLink({ href: /^[a-z][a-z\d+.-]*:/i.test(href) ? href : `https://${href}` }).run();
  else chain.unsetLink().run();
  toolbar.classList.remove('linking');
});
linkInput.addEventListener('keydown', (event) => {
  if (event.key === 'Escape') {
    toolbar.classList.remove('linking');
    editor?.commands.focus();
  }
});

function hideToolbar() {
  toolbar.classList.remove('shown', 'linking');
}

/** Just above the selection, or below it when there's no room above. */
function placeToolbar(force = false) {
  if (!editor || mouseDown || toolbar.classList.contains('linking')) return;
  const { from, to, empty } = editor.state.selection;
  if (empty || (!editor.isFocused && force !== true)) {
    hideToolbar();
    return;
  }
  const start = editor.view.coordsAtPos(from);
  const end = editor.view.coordsAtPos(to);
  toolbar.classList.add('shown');
  const height = toolbar.offsetHeight;
  const width = toolbar.offsetWidth;
  let top = start.top - height - 8;
  if (top < 8) top = end.bottom + 8;
  const left = Math.max(8, Math.min(start.left - 8, window.innerWidth - width - 8));
  toolbar.style.transform = `translate(${Math.round(left)}px, ${Math.round(top + window.scrollY)}px)`;
}

document.addEventListener('mousedown', (event) => {
  if (!toolbar.contains(event.target as Node)) {
    mouseDown = true;
    if (!toolbar.classList.contains('linking')) hideToolbar();
  }
});
document.addEventListener('mouseup', () => {
  if (!mouseDown) return;
  mouseDown = false;
  // After the click has settled the selection.
  setTimeout(() => placeToolbar(), 0);
});
window.addEventListener('scroll', () => {
  if (toolbar.classList.contains('shown')) placeToolbar(true);
}, { passive: true });
window.addEventListener('resize', () => placeToolbar(true));

// The app saves when the note loses focus (the GPUI editor's Blur).
document.addEventListener('focusout', (event) => {
  if (mode !== 'edit') return;
  const next = event.relatedTarget as Node | null;
  if (next && root.contains(next)) return;
  if (toolbar.contains(next)) return;
  hideToolbar();
  post({ type: 'blur' });
});

// --- API for the app ---

const api = {
  show({ mode: next, title, markdown, focus, fields: shown }: Show) {
    // Switching between reading and editing keeps the reader's place.
    const scroll = window.scrollY;
    teardown();
    mode = next;
    fields = shown;
    titleField.placeholder = shown?.titlePlaceholder ?? 'Title';
    titleField.readOnly = shown?.titleReadOnly ?? false;
    // Read mode shows a skill's line as it is, or nothing when it has none.
    descriptionField.hidden = !shown || (next === 'read' && !shown.description);
    descriptionField.readOnly = next === 'read';
    lastMarkdown = markdown;
    root.dataset.mode = next;
    if (shown) {
      descriptionField.placeholder = shown.descriptionPlaceholder;
      descriptionField.maxLength = shown.descriptionMax;
      descriptionField.value = shown.description;
      sizeTextarea(descriptionField);
    }
    if (next === 'read') {
      titleView.textContent = title;
      titleView.hidden = !title;
      renderReader(markdown);
      window.scrollTo(0, scroll);
      return;
    }
    titleField.value = title;
    sizeTextarea(titleField);
    const html = shown ? null : markdownToNoteHtml(markdown);
    if (html === null) createPlain(markdown);
    else createEditor(html);
    window.scrollTo(0, scroll);
    if (focus === 'title') titleField.focus({ preventScroll: true });
    else if (focus === 'description') descriptionField.focus({ preventScroll: true });
    else if (focus === 'body') {
      if (editor) setTimeout(() => editor?.commands.focus('end', { scrollIntoView: false }), 0);
      else plain?.focus({ preventScroll: true });
    }
  },

  /** Theme and type: CSS custom properties for :root. */
  setTheme(css: string) {
    let tag = document.getElementById('theme');
    if (!tag) {
      tag = document.createElement('style');
      tag.id = 'theme';
      document.head.appendChild(tag);
    }
    tag.textContent = `:root{${css}}`;
  },

  /** Selects the first `text` in the body and shows the toolbar over it (screenshots). */
  selectForTest(text: string) {
    if (!editor) return false;
    let found = -1;
    editor.state.doc.descendants((node, pos) => {
      if (found < 0 && node.isText && node.text!.includes(text)) found = pos + node.text!.indexOf(text);
    });
    if (found < 0) return false;
    editor.commands.setTextSelection({ from: found, to: found + text.length });
    placeToolbar(true);
    return true;
  },

  /** What the page shows and how far it's scrolled, as JSON (the app's scripted checks). */
  stateForTest() {
    const doc = document.documentElement;
    return JSON.stringify({
      mode,
      // What read mode drew, by tag: `h2: 3, strong: 5, …`.
      rendered: mode === 'read' ? Array.from(bodyHost.querySelectorAll('*')).reduce<Record<string, number>>((counts, node) => {
        const tag = node.tagName.toLowerCase();
        counts[tag] = (counts[tag] ?? 0) + 1;
        return counts;
      }, {}) : {},
      title: titleField.value,
      description: descriptionField.value,
      body: bodyMarkdown(),
      focused: document.activeElement?.id || (editor?.isFocused ? 'body' : ''),
      bodyHeight: bodyHost.getBoundingClientRect().height,
      scrollY: window.scrollY,
      viewport: window.innerHeight,
      pageHeight: doc.scrollHeight,
    });
  },

  /** Types into the title and body like a keyboard (the app's smoke test). */
  typeForTest(title: string, body: string) {
    titleField.focus();
    titleField.value += title;
    titleField.dispatchEvent(new Event('input'));
    if (editor) editor.chain().focus('end').insertContent(body).run();
    else if (plain) {
      plain.value += body;
      plain.dispatchEvent(new Event('input'));
    }
  },
};

window.FiloNote = api;
// The app sends its commands once the page can take them (not on a timer: a
// window behind others throttles those).
post({ type: 'ready' });
