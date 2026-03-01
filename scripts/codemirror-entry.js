/**
 * CodeMirror 6 bundle entry point.
 *
 * Imports all needed CodeMirror modules and assigns them to
 * window.CodeMirror as a flat namespace. Bundled via esbuild
 * as an IIFE — run scripts/bundle-codemirror.sh to produce
 * static/codemirror.js.
 */

import { EditorView, keymap, lineNumbers, highlightActiveLineGutter, highlightSpecialChars, drawSelection, highlightActiveLine, rectangularSelection, crosshairCursor } from '@codemirror/view';
import { EditorState, Compartment } from '@codemirror/state';
import { defaultKeymap, history, historyKeymap, indentWithTab } from '@codemirror/commands';
import { syntaxHighlighting, defaultHighlightStyle, indentOnInput, bracketMatching, foldGutter, foldKeymap, HighlightStyle } from '@codemirror/language';
import { searchKeymap, highlightSelectionMatches } from '@codemirror/search';
import { autocompletion, completionKeymap, closeBrackets, closeBracketsKeymap } from '@codemirror/autocomplete';

// Language support
import { javascript } from '@codemirror/lang-javascript';
import { json } from '@codemirror/lang-json';
import { html } from '@codemirror/lang-html';
import { css } from '@codemirror/lang-css';
import { python } from '@codemirror/lang-python';

import { tags } from '@lezer/highlight';

window.CodeMirror = {
  // view
  EditorView,
  keymap,
  lineNumbers,
  highlightActiveLineGutter,
  highlightSpecialChars,
  drawSelection,
  highlightActiveLine,
  rectangularSelection,
  crosshairCursor,

  // state
  EditorState,
  Compartment,

  // commands
  defaultKeymap,
  history,
  historyKeymap,
  indentWithTab,

  // language
  syntaxHighlighting,
  defaultHighlightStyle,
  HighlightStyle,
  indentOnInput,
  bracketMatching,
  foldGutter,
  foldKeymap,
  tags,

  // search
  searchKeymap,
  highlightSelectionMatches,

  // autocomplete
  autocompletion,
  completionKeymap,
  closeBrackets,
  closeBracketsKeymap,

  // language modes
  javascript,
  json,
  html,
  css,
  python,
};
