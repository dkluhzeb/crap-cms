/**
 * Shared utilities for `<crap-*>` components.
 *
 * Re-export of every util submodule. Components import per-concern
 * (`import { readCsrfCookie } from './util/cookies.js'`) — this index
 * exists for callers that want one import point.
 *
 * @module util
 */

export { readCookie, readCsrfCookie } from './cookies.js';
export { discoverSingleton } from './discover.js';
export { getHttpVerb } from './htmx.js';
export { parseJsonAttribute, readDataIsland } from './json.js';
export { toast } from './toast.js';
