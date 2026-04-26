/**
 * <crap-code> — CodeMirror 6-based code editor.
 *
 * Wraps a hidden `<textarea>` with a code editor. The textarea remains
 * the form submission source — the editor syncs content back on every
 * change. Falls back to showing the plain textarea if CodeMirror is
 * unavailable.
 *
 * Requires `window.CodeMirror` (loaded via `codemirror.js` IIFE bundle).
 *
 * @example
 * <crap-code data-language="json">
 *   <textarea name="content" hidden>...</textarea>
 * </crap-code>
 *
 * @module code
 */

import { css } from './css.js';
import { h } from './h.js';

const sheet = css`
  :host { display: block; }

  .code-editor {
    border: 1px solid var(--input-border, #e0e0e0);
    border-radius: var(--radius-md, 6px);
    background: var(--input-bg, #fff);
    box-shadow: var(--shadow-sm, 0 1px 2px rgba(0,0,0,0.04));
    overflow: hidden;
  }
  .code-editor:focus-within {
    border-color: var(--color-primary, #1677ff);
    box-shadow: 0 0 0 2px var(--color-primary-bg, rgba(22, 119, 255, 0.06));
  }
  .code-editor .cm-editor {
    min-height: 12.5rem;
    max-height: 37.5rem;
    overflow: auto;
  }
  .code-editor .cm-editor.cm-focused { outline: none; }
  .code-editor .cm-scroller {
    font-family: monospace;
    line-height: 1.5;
  }
`;

/**
 * Map of `data-language` values to factory names on the CodeMirror
 * namespace. A language without a matching entry simply has no syntax
 * extension; the editor still works.
 */
const LANGUAGE_FACTORIES = {
  javascript: 'javascript',
  js: 'javascript',
  json: 'json',
  html: 'html',
  css: 'css',
  python: 'python',
  py: 'python',
};

/**
 * Theme-aware syntax-highlight style. Each token tag binds to a
 * `--code-*` custom property (defined per theme in `styles.css` /
 * `themes.css`), with a sensible light-theme hex fallback so the
 * editor still looks reasonable on pages where `:root` hasn't loaded.
 *
 * Built lazily on first use so we read `window.CodeMirror.tags` after
 * the bundle has loaded (this module is parsed before
 * `connectedCallback` runs, but `code.js` could in theory be evaluated
 * before the bundle's IIFE — be defensive).
 *
 * @type {any|null}
 */
let _highlightStyle = null;

/** @param {any} CM */
function getHighlightStyle(CM) {
  if (_highlightStyle) return _highlightStyle;
  if (!CM.HighlightStyle || !CM.tags) return CM.defaultHighlightStyle;
  const t = CM.tags;
  _highlightStyle = CM.HighlightStyle.define([
    { tag: t.keyword, color: 'var(--code-keyword, #708)' },
    { tag: [t.string, t.special(t.string)], color: 'var(--code-string, #a11)' },
    { tag: t.number, color: 'var(--code-number, #164)' },
    { tag: t.comment, color: 'var(--code-comment, #888)', fontStyle: 'italic' },
    { tag: [t.atom, t.bool, t.null], color: 'var(--code-atom, #219)' },
    { tag: t.propertyName, color: 'var(--code-property, #00f)' },
    { tag: t.function(t.variableName), color: 'var(--code-function, #00c)' },
    { tag: t.definition(t.variableName), color: 'var(--code-definition, #00f)' },
    { tag: [t.typeName, t.className], color: 'var(--code-type, #085)' },
    { tag: t.operator, color: 'var(--code-operator, #708)' },
    { tag: t.regexp, color: 'var(--code-regexp, #a11)' },
    { tag: t.meta, color: 'var(--code-meta, #888)' },
    { tag: t.tagName, color: 'var(--code-tag, #708)' },
    { tag: t.attributeName, color: 'var(--code-attribute, #00c)' },
    { tag: t.heading, color: 'var(--code-heading, #708)', fontWeight: 'bold' },
    { tag: t.link, color: 'var(--code-link, #00c)', textDecoration: 'underline' },
  ]);
  return _highlightStyle;
}

/**
 * Editor theme bound to the admin CSS custom properties so the editor
 * follows the active theme. Static spec — same for every instance.
 */
const THEME_SPEC = {
  '&': {
    fontSize: 'var(--text-sm, 0.8125rem)',
    fontFamily: 'monospace',
  },
  '.cm-content': {
    fontFamily: 'monospace',
    padding: 'var(--space-sm, 0.5rem) 0',
  },
  '.cm-gutters': {
    backgroundColor: 'var(--bg-surface, #fafafa)',
    borderRight: '1px solid var(--border-color, #e0e0e0)',
    color: 'var(--text-tertiary, rgba(0,0,0,0.45))',
  },
  '.cm-activeLineGutter': {
    backgroundColor: 'var(--bg-hover, rgba(0,0,0,0.04))',
  },
  '&.cm-focused .cm-cursor': {
    borderLeftColor: 'var(--text-primary, rgba(0,0,0,0.88))',
  },
  '&.cm-focused .cm-selectionBackground, .cm-selectionBackground': {
    backgroundColor: 'var(--color-primary-bg, rgba(22,119,255,0.12))',
  },
  '.cm-activeLine': {
    backgroundColor: 'var(--bg-hover, rgba(0,0,0,0.02))',
  },
};

/**
 * Resolve the CodeMirror language extension for a `data-language` value,
 * or `null` if the language isn't recognised.
 *
 * @param {any} CM CodeMirror namespace
 * @param {string} language
 */
function getLanguageExtension(CM, language) {
  const factory = LANGUAGE_FACTORIES[language];
  return factory ? CM[factory]() : null;
}

/**
 * Build the editor's extension list for one instance.
 *
 * @param {any} CM
 * @param {HTMLTextAreaElement} textarea Sync target for `docChanged` updates.
 * @param {string} language
 * @param {boolean} readonly
 */
function buildExtensions(CM, textarea, language, readonly) {
  const ext = [
    CM.lineNumbers(),
    CM.highlightActiveLineGutter(),
    CM.highlightSpecialChars(),
    CM.history(),
    CM.foldGutter(),
    CM.drawSelection(),
    CM.EditorState.allowMultipleSelections.of(true),
    CM.indentOnInput(),
    CM.bracketMatching(),
    CM.closeBrackets(),
    CM.autocompletion(),
    CM.rectangularSelection(),
    CM.crosshairCursor(),
    CM.highlightActiveLine(),
    CM.highlightSelectionMatches(),
    CM.keymap.of([
      ...CM.closeBracketsKeymap,
      ...CM.defaultKeymap,
      ...CM.searchKeymap,
      ...CM.historyKeymap,
      ...CM.foldKeymap,
      ...CM.completionKeymap,
      CM.indentWithTab,
    ]),
  ];

  // Language must register before `syntaxHighlighting` so the parser's
  // tokens are available when the highlight style is applied.
  const langExt = getLanguageExtension(CM, language);
  if (langExt) ext.push(langExt);
  ext.push(CM.syntaxHighlighting(getHighlightStyle(CM)));

  if (readonly) ext.push(CM.EditorState.readOnly.of(true));

  ext.push(
    CM.EditorView.updateListener.of(
      /** @param {any} update */ (update) => {
        if (update.docChanged) textarea.value = update.state.doc.toString();
      },
    ),
    CM.EditorView.theme(THEME_SPEC),
  );
  return ext;
}

class CrapCode extends HTMLElement {
  constructor() {
    super();
    /** @type {any} */
    this._view = null;
    this.attachShadow({ mode: 'open' });
  }

  connectedCallback() {
    // Idempotency guard: skip re-init on DOM moves (e.g. array row drag-and-drop).
    if (this._view) return;

    const textarea = /** @type {HTMLTextAreaElement|null} */ (this.querySelector('textarea'));
    if (!textarea) return;

    // Graceful degradation: no CodeMirror available → leave the textarea visible.
    const CM = /** @type {any} */ (window).CodeMirror;
    if (!CM) {
      textarea.hidden = false;
      return;
    }

    textarea.hidden = true;
    const language = this.getAttribute('data-language') || 'json';
    const readonly = textarea.hasAttribute('readonly');
    const extensions = buildExtensions(CM, textarea, language, readonly);

    const editorEl = h('div', { class: 'code-editor' });
    const root = /** @type {ShadowRoot} */ (this.shadowRoot);
    root.adoptedStyleSheets = [sheet];
    root.append(editorEl);

    this._view = new CM.EditorView({
      state: CM.EditorState.create({ doc: textarea.value || '', extensions }),
      parent: editorEl,
    });
  }

  /*
   * Intentionally empty: DOM moves (drag-and-drop reordering of array
   * rows) trigger disconnect+reconnect, and we want to preserve editor
   * state across them. Do NOT destroy the view here.
   */
  disconnectedCallback() {}
}

customElements.define('crap-code', CrapCode);
