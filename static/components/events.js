/**
 * Central event vocabulary for `<crap-*>` web components.
 *
 * Every cross-component custom event has its name exported as a named
 * constant here, plus a JSDoc typedef describing the event's `detail`
 * shape. Components import these instead of re-typing the string
 * literal in each file. Three reasons this matters:
 *
 *   1. **Refactor safety** — renaming an event becomes a single-line
 *      change here; old strings won't compile (well, won't lint) once
 *      every site imports.
 *   2. **Stable surface** — these constants ARE the public event API
 *      that overlays + custom widgets can program against. Strings
 *      defined inline anywhere else are component-internal and free
 *      to change.
 *   3. **Discoverability** — one place to look for "what events does
 *      crap-cms's admin emit?".
 *
 * ## Event taxonomy
 *
 * - **Singleton-request events** (`*-request`): consumers dispatch on
 *   `document` to ask the page's singleton component to do something
 *   (open a drawer, show a confirm dialog, etc.). The singleton listens
 *   on `document`, reads `detail`, and may write back `detail.instance`
 *   so callers can grab a handle (see {@link util/discover.js}).
 * - **Bubbling change events** (`crap:change`): emitted by form-shaped
 *   components when their value changes; the surrounding `<crap-dirty-form>`
 *   listens to mark the form dirty.
 * - **Local notifications** (`crap:pick`, `crap:request-add-block`):
 *   parent ↔ child within a single composite component tree.
 *
 * @module events
 * @stability stable
 */

// ── Singleton-request events ─────────────────────────────────────

/**
 * Dispatch on `document` to open the toast layer with a message.
 *
 * @example
 *   document.dispatchEvent(new CustomEvent(EV_TOAST_REQUEST, {
 *     detail: { message: 'Saved', type: 'success' }
 *   }));
 *
 * @typedef {Object} ToastDetail
 * @property {string} message
 * @property {'info'|'success'|'warning'|'error'} [type]
 * @property {number} [duration] — ms; 0 to keep the toast open.
 */
export const EV_TOAST_REQUEST = 'crap:toast-request';

/**
 * Dispatch to open the side drawer with arbitrary content.
 *
 * @example
 *   document.dispatchEvent(new CustomEvent(EV_DRAWER_REQUEST, {
 *     detail: { url: '/admin/collections/posts/123', title: 'Edit post' }
 *   }));
 *
 * Pass `{ detail: {} }` to discover the singleton instance — the
 * drawer writes back `detail.instance = this`. See `util/discover.js`.
 */
export const EV_DRAWER_REQUEST = 'crap:drawer-request';

/**
 * Dispatch to ask a confirm dialog from the user. The detail's
 * `confirm` callback is invoked with `true|false`.
 *
 * @example
 *   document.dispatchEvent(new CustomEvent(EV_CONFIRM_DIALOG_REQUEST, {
 *     detail: { message: 'Discard changes?', onConfirm: () => …, onCancel: () => … }
 *   }));
 */
export const EV_CONFIRM_DIALOG_REQUEST = 'crap:confirm-dialog-request';

/** Dispatch to open the delete-confirmation dialog. */
export const EV_DELETE_DIALOG_REQUEST = 'crap:delete-dialog-request';

/** Dispatch to open the inline create-panel drawer. */
export const EV_CREATE_PANEL_REQUEST = 'crap:create-panel-request';

// ── Bubbling change notifications ────────────────────────────────

/**
 * Bubbling event fired by form-shaped components (`<crap-tags>`,
 * `<crap-code>`, `<crap-relationship-search>`, `<crap-uploads>`, …)
 * when their value changes. Listened to by `<crap-dirty-form>` to
 * mark the surrounding form as having unsaved changes.
 *
 * Plain `Event` (no `detail`) — the value is on the host element.
 */
export const EV_CHANGE = 'crap:change';

// ── Local notifications ──────────────────────────────────────────

/**
 * Emitted by the relationship picker when the user selects an item.
 * Listened to by the parent search field to set its value.
 */
export const EV_PICK = 'crap:pick';

/**
 * Emitted by `<crap-block-picker>` when a block type is chosen,
 * picked up by the array-fields container to insert a new block row.
 */
export const EV_REQUEST_ADD_BLOCK = 'crap:request-add-block';
