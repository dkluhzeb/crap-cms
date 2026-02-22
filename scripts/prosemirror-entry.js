/**
 * ProseMirror bundle entry point.
 *
 * Imports all needed ProseMirror modules and assigns them to
 * window.ProseMirror as a flat namespace. Bundled via esbuild
 * as an IIFE — run scripts/bundle-prosemirror.sh to produce
 * static/prosemirror.js.
 */

import { Schema, DOMParser, DOMSerializer, Fragment, Slice, Node } from 'prosemirror-model';
import { EditorState, Plugin, PluginKey, Transaction, TextSelection } from 'prosemirror-state';
import { EditorView } from 'prosemirror-view';
import { schema as basicSchema } from 'prosemirror-schema-basic';
import { addListNodes } from 'prosemirror-schema-list';
import { history, undo, redo } from 'prosemirror-history';
import { keymap } from 'prosemirror-keymap';
import {
  baseKeymap,
  toggleMark,
  setBlockType,
  wrapIn,
  chainCommands,
  exitCode,
  joinUp,
  joinDown,
  lift,
  selectParentNode,
} from 'prosemirror-commands';
import { wrapInList, splitListItem, liftListItem, sinkListItem } from 'prosemirror-schema-list';
import { dropCursor } from 'prosemirror-dropcursor';
import { gapCursor } from 'prosemirror-gapcursor';
import { inputRules, wrappingInputRule, textblockTypeInputRule, smartQuotes, emDash, ellipsis } from 'prosemirror-inputrules';

window.ProseMirror = {
  // model
  Schema,
  DOMParser,
  DOMSerializer,
  Fragment,
  Slice,
  Node,
  // state
  EditorState,
  Plugin,
  PluginKey,
  Transaction,
  TextSelection,
  // view
  EditorView,
  // schema
  basicSchema,
  addListNodes,
  // history
  history,
  undo,
  redo,
  // keymap
  keymap,
  // commands
  baseKeymap,
  toggleMark,
  setBlockType,
  wrapIn,
  chainCommands,
  exitCode,
  joinUp,
  joinDown,
  lift,
  selectParentNode,
  // list commands
  wrapInList,
  splitListItem,
  liftListItem,
  sinkListItem,
  // plugins
  dropCursor,
  gapCursor,
  // input rules
  inputRules,
  wrappingInputRule,
  textblockTypeInputRule,
  smartQuotes,
  emDash,
  ellipsis,
};
