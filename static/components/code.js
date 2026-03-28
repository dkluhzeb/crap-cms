/**
 * <crap-code> — CodeMirror 6-based code editor.
 *
 * Wraps a hidden <textarea> with a code editor. The textarea remains
 * the form submission source — the editor syncs content back on every change.
 *
 * Requires `window.CodeMirror` (loaded via codemirror.js IIFE bundle).
 * Falls back to showing the plain textarea if CodeMirror is unavailable.
 *
 * @example
 * <crap-code data-language="json">
 *   <textarea name="content" style="display:none">...</textarea>
 * </crap-code>
 */
class CrapCode extends HTMLElement {
  constructor() {
    super();

    /** @type {any} */
    this._view = null;

    this.attachShadow({ mode: 'open' });
  }

  connectedCallback() {
    const CM = /** @type {any} */ (window).CodeMirror;
    /** @type {HTMLTextAreaElement | null} */
    const textarea = this.querySelector('textarea');

    // Graceful degradation: no CodeMirror -> show plain textarea
    if (!CM || !textarea) {
      if (textarea) textarea.style.display = '';
      return;
    }

    textarea.style.display = 'none';

    const isReadonly = textarea.hasAttribute('readonly');
    const language = this.getAttribute('data-language') || 'json';

    // Build extensions list
    const extensions = [
      CM.lineNumbers(),
      CM.highlightActiveLineGutter(),
      CM.highlightSpecialChars(),
      CM.history(),
      CM.foldGutter(),
      CM.drawSelection(),
      CM.EditorState.allowMultipleSelections.of(true),
      CM.indentOnInput(),
      CM.syntaxHighlighting(CM.defaultHighlightStyle, { fallback: true }),
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

    // Add language extension
    const langExt = this._getLanguageExtension(CM, language);
    if (langExt) extensions.push(langExt);

    // Readonly
    if (isReadonly) {
      extensions.push(CM.EditorState.readOnly.of(true));
    }

    // Sync changes back to textarea
    extensions.push(CM.EditorView.updateListener.of(
      /** @param {any} update */ (update) => {
        if (update.docChanged) {
          textarea.value = update.state.doc.toString();
        }
      }
    ));

    // Theme: match admin CSS variables
    extensions.push(CM.EditorView.theme({
      '&': {
        fontSize: 'var(--text-sm, 0.8125rem)',
        fontFamily: 'monospace',
      },
      '.cm-content': {
        fontFamily: 'monospace',
        padding: 'var(--space-sm, 0.5rem) 0',
      },
      '.cm-gutters': {
        backgroundColor: 'var(--bg-secondary, #fafafa)',
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
    }));

    // Render Shadow DOM
    this.shadowRoot.innerHTML = `
      <style>${CrapCode._styles()}</style>
      <div class="code-editor"></div>
    `;

    const editorEl = this.shadowRoot.querySelector('.code-editor');

    this._view = new CM.EditorView({
      state: CM.EditorState.create({
        doc: textarea.value || '',
        extensions,
      }),
      parent: editorEl,
    });
  }

  disconnectedCallback() {
    if (this._view) {
      this._view.destroy();
      this._view = null;
    }
  }

  /**
   * Get the language extension for a given language string.
   * @param {any} CM - CodeMirror namespace
   * @param {string} language - Language identifier
   * @returns {any} Language extension or null
   */
  _getLanguageExtension(CM, language) {
    switch (language) {
      case 'javascript':
      case 'js':
        return CM.javascript();
      case 'json':
        return CM.json();
      case 'html':
        return CM.html();
      case 'css':
        return CM.css();
      case 'python':
      case 'py':
        return CM.python();
      default:
        return null;
    }
  }

  /**
   * Shadow DOM styles. Uses CSS custom properties from :root (penetrate shadow boundary).
   * @returns {string}
   */
  static _styles() {
    return `
      :host {
        display: block;
      }

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

      .code-editor .cm-editor.cm-focused {
        outline: none;
      }

      .code-editor .cm-scroller {
        font-family: monospace;
        line-height: 1.5;
      }
    `;
  }
}

customElements.define('crap-code', CrapCode);
