/**
 * Constructable stylesheet for `<crap-richtext>`.
 *
 * Adopted into the component's shadow root via `adoptedStyleSheets`.
 * Lives in its own module so the main component file stays readable.
 *
 * @module richtext/styles
 */

import { css } from '../css.js';

export const sheet = css`
  :host { display: block; }

  .richtext {
    border: 1px solid var(--input-border, #e0e0e0);
    border-radius: var(--radius-md, 6px);
    background: var(--input-bg, #fff);
    box-shadow: var(--shadow-sm, 0 1px 2px rgba(0,0,0,0.04));
    transition: border-color var(--transition-fast, 0.15s ease),
                box-shadow var(--transition-fast, 0.15s ease);
  }
  .richtext:focus-within {
    border-color: var(--color-primary, #1677ff);
    box-shadow: 0 0 0 2px var(--color-primary-bg, rgba(22, 119, 255, 0.06));
  }
  .richtext__toolbar {
    display: flex;
    align-items: center;
    gap: var(--space-xs, 0.25rem);
    padding: var(--space-xs, 0.25rem) var(--space-sm, 0.5rem);
    border-bottom: 1px solid var(--input-border, #e0e0e0);
    background: var(--bg-surface, #fafafa);
    border-radius: var(--radius-md, 6px) var(--radius-md, 6px) 0 0;
    flex-wrap: wrap;
  }
  .richtext__toolbar-group {
    display: flex;
    gap: 2px;
    padding: 0 var(--space-xs, 0.25rem);
    border-right: 1px solid var(--input-border, #e0e0e0);
  }
  .richtext__toolbar-group:last-child { border-right: none; }
  .richtext__toolbar button {
    padding: var(--space-2xs, 2px) var(--space-sm, 0.5rem);
    border: 1px solid transparent;
    border-radius: var(--radius-sm, 4px);
    background: transparent;
    color: var(--text-primary, rgba(0,0,0,0.88));
    font-size: var(--text-sm, 0.8125rem);
    font-family: inherit;
    cursor: pointer;
    min-width: 1.75rem;
    transition: background var(--transition-fast, 0.15s ease);
  }
  .richtext__toolbar button:hover {
    background: var(--bg-hover, rgba(0,0,0,0.04));
  }
  .richtext__toolbar button.active {
    background: var(--color-primary-bg, rgba(22,119,255,0.1));
    color: var(--color-primary, #1677ff);
  }
  .richtext__toolbar button strong { font-weight: 700; }
  .richtext__toolbar button em { font-style: italic; }
  .richtext__toolbar button code {
    font-family: monospace;
    font-size: var(--text-xs, 0.75rem);
  }

  .richtext__editor {
    padding: var(--space-md, 0.75rem);
    min-height: 7.5rem;
    max-height: 30rem;
    overflow-y: auto;
    font-size: var(--text-base, 0.875rem);
    line-height: 1.6;
    color: var(--text-primary, rgba(0,0,0,0.88));
    resize: vertical;
  }
  .richtext--no-resize .richtext__editor { resize: none; }
  .richtext__editor:focus { outline: none; }

  .ProseMirror {
    outline: none;
    min-height: 4rem;
  }
  .ProseMirror p {
    margin: 0 0 var(--space-sm, 0.5rem);
    line-height: 1.6;
  }
  .ProseMirror p:last-child { margin-bottom: 0; }
  .ProseMirror h1 {
    font-size: var(--text-3xl, 1.875rem);
    font-weight: 700;
    margin: var(--space-md, 0.75rem) 0 var(--space-sm, 0.5rem);
    line-height: 1.2;
    color: var(--text-primary, rgba(0,0,0,0.88));
  }
  .ProseMirror h2 {
    font-size: var(--text-2xl, 1.5rem);
    font-weight: 700;
    margin: var(--space-md, 0.75rem) 0 var(--space-sm, 0.5rem);
    line-height: 1.25;
    color: var(--text-primary, rgba(0,0,0,0.88));
  }
  .ProseMirror h3 {
    font-size: var(--text-xl, 1.25rem);
    font-weight: 600;
    margin: var(--space-md, 0.75rem) 0 var(--space-sm, 0.5rem);
    line-height: 1.3;
    color: var(--text-primary, rgba(0,0,0,0.88));
  }
  .ProseMirror h1:first-child,
  .ProseMirror h2:first-child,
  .ProseMirror h3:first-child {
    margin-top: 0;
  }
  .ProseMirror ul, .ProseMirror ol {
    margin: 0 0 var(--space-sm, 0.5rem);
    padding-left: var(--space-2xl, 2rem);
  }
  .ProseMirror li {
    margin-bottom: var(--space-2xs, 2px);
    line-height: 1.6;
  }
  .ProseMirror li > p { margin-bottom: 0; }
  .ProseMirror blockquote {
    margin: 0 0 var(--space-sm, 0.5rem);
    padding-left: var(--space-md, 0.75rem);
    border-left: 4px solid var(--input-border, #e0e0e0);
    color: var(--text-secondary, rgba(0,0,0,0.65));
    font-style: italic;
  }
  .ProseMirror code {
    background: var(--bg-surface, #fafafa);
    padding: 1px var(--space-2xs, 2px);
    border-radius: var(--radius-sm, 4px);
    font-family: monospace;
    font-size: 0.9em;
  }
  .ProseMirror pre {
    background: var(--bg-surface, #fafafa);
    padding: var(--space-md, 0.75rem);
    border-radius: var(--radius-sm, 4px);
    overflow-x: auto;
    margin: 0 0 var(--space-sm, 0.5rem);
    font-family: monospace;
    font-size: var(--text-sm, 0.8125rem);
    line-height: 1.5;
  }
  .ProseMirror pre code {
    background: transparent;
    padding: 0;
    border-radius: 0;
  }
  .ProseMirror hr {
    border: none;
    border-top: 1px solid var(--input-border, #e0e0e0);
    margin: var(--space-md, 0.75rem) 0;
  }
  .ProseMirror a {
    color: var(--color-primary, #1677ff);
    text-decoration: underline;
  }
  .ProseMirror a:hover {
    color: var(--color-primary-hover, #4096ff);
  }

  /* Custom-node atom rendering inside the editor */
  .crap-custom-node {
    display: block;
    padding: var(--space-sm, 0.5rem) var(--space-md, 0.75rem);
    margin: var(--space-sm, 0.5rem) 0;
    background: var(--bg-surface, #fafafa);
    border: 1px solid var(--input-border, #e0e0e0);
    border-radius: var(--radius-sm, 4px);
    cursor: pointer;
    user-select: none;
    transition: background var(--transition-fast, 0.15s ease),
                border-color var(--transition-fast, 0.15s ease);
  }
  .crap-custom-node:hover {
    background: var(--bg-hover, rgba(0,0,0,0.04));
    border-color: var(--color-primary, #1677ff);
  }
  .crap-custom-node.ProseMirror-selectednode {
    background: var(--color-primary-bg, rgba(22,119,255,0.08));
    border-color: var(--color-primary, #1677ff);
    outline: 2px solid var(--color-primary, #1677ff);
    outline-offset: -2px;
  }
  .crap-custom-node--inline {
    display: inline-block;
    padding: 1px var(--space-sm, 0.5rem);
    margin: 0 var(--space-2xs, 2px);
  }
  .crap-custom-node--error {
    border-color: var(--color-danger, #dc2626);
    background: var(--color-danger-bg, rgba(220, 38, 38, 0.04));
  }
  .crap-custom-node__label {
    font-size: var(--text-sm, 0.8125rem);
    font-weight: 600;
    color: var(--text-primary, rgba(0,0,0,0.88));
  }
  .crap-custom-node__attrs {
    font-size: var(--text-xs, 0.75rem);
    color: var(--text-secondary, rgba(0,0,0,0.65));
    margin-left: var(--space-sm, 0.5rem);
    font-family: monospace;
  }

  /* Modal — used by both link insert and custom-node edit */
  .crap-node-modal {
    border: none;
    padding: 0;
    background: transparent;
    overflow: visible;
    max-width: none;
    max-height: none;
  }
  .crap-node-modal::backdrop { background: rgba(0, 0, 0, 0.5); }
  .crap-node-modal__dialog {
    background: var(--bg-elevated, #fff);
    border-radius: var(--radius-md, 6px);
    box-shadow: var(--shadow-lg, 0 16px 48px rgba(0,0,0,0.18));
    min-width: 25rem;
    max-width: 31.25rem;
    overflow: hidden;
  }
  .crap-node-modal__header {
    padding: var(--space-md, 0.75rem) var(--space-lg, 1rem);
    border-bottom: 1px solid var(--input-border, #e0e0e0);
    font-weight: 600;
    color: var(--text-primary, rgba(0,0,0,0.88));
  }
  .crap-node-modal__body {
    padding: var(--space-lg, 1rem);
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-md, 0.75rem);
  }
  .crap-node-modal__field {
    flex: 1 1 100%;
    display: flex;
    flex-direction: column;
    gap: var(--space-xs, 0.25rem);
  }
  .crap-node-modal__field[data-field-width="50"]  { flex: 1 1 calc(50% - var(--space-md, 0.75rem) / 2); min-width: 0; }
  .crap-node-modal__field[data-field-width="33"]  { flex: 1 1 calc(33.33% - var(--space-md, 0.75rem)); min-width: 0; }
  .crap-node-modal__field[data-field-width="25"]  { flex: 1 1 calc(25% - var(--space-md, 0.75rem)); min-width: 0; }
  .crap-node-modal__label {
    font-size: var(--text-sm, 0.8125rem);
    font-weight: 500;
    color: var(--text-primary, rgba(0,0,0,0.88));
  }
  .crap-node-modal__help {
    margin: 0;
    font-size: var(--text-xs, 0.75rem);
    color: var(--text-tertiary, rgba(0,0,0,0.45));
  }
  .crap-node-modal__input {
    width: 100%;
    padding: var(--space-sm, 0.5rem) var(--space-md, 0.75rem);
    border: 1px solid var(--input-border, #e0e0e0);
    border-radius: var(--radius-sm, 4px);
    font-size: var(--text-sm, 0.8125rem);
    font-family: inherit;
    background: var(--input-bg, #fff);
    color: var(--text-primary, rgba(0,0,0,0.88));
    box-sizing: border-box;
  }
  .crap-node-modal__input:focus {
    outline: none;
    border-color: var(--color-primary, #1677ff);
    box-shadow: 0 0 0 2px var(--color-primary-bg, rgba(22,119,255,0.06));
  }
  .crap-node-modal__input--mono {
    font-family: monospace;
    font-size: var(--text-xs, 0.75rem);
  }
  .crap-node-modal__input--error {
    border-color: var(--color-danger, #dc2626);
  }
  .crap-node-modal__input--error:focus {
    border-color: var(--color-danger, #dc2626);
    box-shadow: 0 0 0 2px var(--color-danger-bg, rgba(220, 38, 38, 0.06));
  }
  .crap-node-modal__error {
    margin: var(--space-2xs, 2px) 0 0;
    font-size: var(--text-xs, 0.75rem);
    color: var(--color-danger, #dc3545);
  }
  .crap-node-modal__checkbox {
    display: flex;
    align-items: center;
    gap: var(--space-xs, 0.25rem);
    font-size: var(--text-sm, 0.8125rem);
    color: var(--text-primary, rgba(0,0,0,0.88));
    cursor: pointer;
  }
  .crap-node-modal__radio-group {
    display: flex;
    flex-direction: column;
    gap: var(--space-xs, 0.25rem);
  }
  .crap-node-modal__radio {
    display: flex;
    align-items: center;
    gap: var(--space-xs, 0.25rem);
    font-size: var(--text-sm, 0.8125rem);
    cursor: pointer;
  }
  .crap-node-modal__footer {
    padding: var(--space-md, 0.75rem) var(--space-lg, 1rem);
    border-top: 1px solid var(--input-border, #e0e0e0);
    display: flex;
    justify-content: flex-end;
    gap: var(--space-sm, 0.5rem);
    background: var(--bg-surface, #fafafa);
  }
  .crap-node-modal__footer--with-remove {
    justify-content: space-between;
  }
  .crap-node-modal__footer--with-remove .crap-node-modal__btn--danger {
    margin-right: auto;
  }
  .crap-node-modal__btn {
    padding: var(--space-xs, 0.25rem) var(--space-md, 0.75rem);
    border: 1px solid var(--input-border, #e0e0e0);
    border-radius: var(--radius-sm, 4px);
    background: var(--bg-elevated, #fff);
    font-size: var(--text-sm, 0.8125rem);
    font-family: inherit;
    cursor: pointer;
    transition: background var(--transition-fast, 0.15s ease);
  }
  .crap-node-modal__btn:hover { background: var(--bg-hover, rgba(0,0,0,0.04)); }
  .crap-node-modal__btn--ok {
    background: var(--color-primary, #1677ff);
    color: var(--text-on-primary, #fff);
    border-color: var(--color-primary, #1677ff);
  }
  .crap-node-modal__btn--ok:hover {
    background: var(--color-primary-hover, #4096ff);
    border-color: var(--color-primary-hover, #4096ff);
  }
  .crap-node-modal__btn--danger {
    color: var(--color-danger, #dc3545);
    border-color: var(--color-danger, #dc3545);
  }
  .crap-node-modal__btn--danger:hover {
    background: var(--color-danger, #dc3545);
    color: var(--text-on-primary, #fff);
  }
  .crap-node-modal__btn:disabled {
    opacity: 0.6;
    cursor: not-allowed;
  }
`;
