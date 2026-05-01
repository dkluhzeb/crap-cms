/**
 * The `window.crap` convenience namespace.
 *
 * Sugar over the canonical event-discovery + module APIs. Inline
 * templates and console use can call `window.crap.toast({...})`,
 * `window.crap.confirm('…')`, etc., without boilerplate.
 *
 * **This is NOT the canonical API.** The library's primary contract
 * lives in:
 *  - Custom-event discovery (`crap:<name>-request` with `detail.instance`).
 *  - Per-module imports (`import { toast } from './util/toast.js'`).
 *
 * `window.crap` exists so JS that lives outside the module graph
 * (inline scripts in `<script nonce="…">` blocks, browser console,
 * third-party overlays) can do the same things ergonomically.
 *
 * @module global
 * @stability internal
 */

import {
  EV_CONFIRM_DIALOG_REQUEST,
  EV_CREATE_PANEL_REQUEST,
  EV_DELETE_DIALOG_REQUEST,
  EV_DRAWER_REQUEST,
} from '../events.js';
import { applyTheme, getTheme, setTheme } from '../theme.js';
import { readCsrfCookie } from './util/cookies.js';
import { discoverSingleton } from './util/discover.js';
import { toast as utilToast } from './util/toast.js';

/**
 * @typedef {{
 *   toast: typeof utilToast,
 *   confirm: (message: string, opts?: { confirmLabel?: string, cancelLabel?: string }) => Promise<boolean>,
 *   drawer: { open: (opts: any) => void, close: () => void },
 *   deleteDialog: { open: (opts: any) => void },
 *   createPanel: { open: (opts: any) => void, close: () => void },
 *   theme: { get: () => string, set: (theme: string) => void, apply: (theme: string) => void },
 *   csrf: () => string,
 * }} CrapNamespace
 */

/** @type {CrapNamespace} */
const crap = {
  toast: utilToast,

  async confirm(message, opts) {
    const dialog = discoverSingleton(EV_CONFIRM_DIALOG_REQUEST);
    if (!dialog) return window.confirm(message);
    return dialog.prompt(message, opts);
  },

  drawer: {
    open(opts) {
      discoverSingleton(EV_DRAWER_REQUEST)?.open(opts);
    },
    close() {
      discoverSingleton(EV_DRAWER_REQUEST)?.close();
    },
  },

  deleteDialog: {
    open(opts) {
      discoverSingleton(EV_DELETE_DIALOG_REQUEST)?.open(opts);
    },
  },

  createPanel: {
    open(opts) {
      discoverSingleton(EV_CREATE_PANEL_REQUEST)?.open(opts);
    },
    close() {
      discoverSingleton(EV_CREATE_PANEL_REQUEST)?.close();
    },
  },

  theme: {
    get: getTheme,
    set: setTheme,
    apply: applyTheme,
  },

  csrf: readCsrfCookie,
};

/** Expose under `window.crap` for inline-template + console use. */
/** @type {any} */ (window).crap = crap;
