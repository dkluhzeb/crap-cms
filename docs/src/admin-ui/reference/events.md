# Events

Cross-component custom events emitted by the built-in `<crap-*>` web
components form a **stable public API**. The full vocabulary is
declared in [`static/components/events.js`](https://github.com/dkluhs/crap-cms/blob/main/static/components/events.js)
as exported `EV_*` constants. Overlay authors and custom-component
authors can:

- **Listen** for these events to react to admin actions (audit
  logging, analytics, validation hooks).
- **Dispatch** them to drive built-in singletons (open the toast
  layer, request a confirm dialog, populate the side drawer).

This is the recommended path for **adding behavior** to built-in
components without replacing them. See the
[admin UI overview](../index.md#when-to-use-what) for when to use
this pattern vs. an override.

## Three event categories

Events fall into three categories with different intents and
listening conventions:

| Category | Pattern | Where to dispatch | Where to listen |
|---|---|---|---|
| **Singleton-request** | `*-request` | `document` | the singleton element listens on `document` |
| **Change notifications** | `crap:change` | the form-field host element | the surrounding `<crap-dirty-form>` listens (bubbles up) |
| **Local notifications** | inside a composite | child element | parent element in the same component tree |

## Singleton-request events

These events drive the **page-mounted singleton components**
(`<crap-toast>`, `<crap-drawer>`, etc.). Dispatch on `document` and
the singleton picks the event up.

### `EV_TOAST_REQUEST` — `crap:toast-request`

Open the toast layer with a message.

```js
import { EV_TOAST_REQUEST } from '/static/components/events.js';

document.dispatchEvent(new CustomEvent(EV_TOAST_REQUEST, {
  detail: { message: 'Saved', type: 'success' },
}));
```

**`detail` shape:**

| Field | Type | Required | Notes |
|---|---|---|---|
| `message` | `string` | yes | The message to display. |
| `type` | `'info' \| 'success' \| 'warning' \| 'error'` | no | Default `'info'`. Controls colour and icon. |
| `duration` | `number` | no | Auto-dismiss after N ms. `0` keeps it open until manually dismissed. Default 3500. |

**Convenience wrapper** — the canonical helper is `window.crap.toast({...})`, which dispatches the event for you.

### `EV_DRAWER_REQUEST` — `crap:drawer-request`

Open the right-side drawer with arbitrary content.

```js
document.dispatchEvent(new CustomEvent(EV_DRAWER_REQUEST, {
  detail: { url: '/admin/collections/posts/123', title: 'Edit post' },
}));
```

Pass `{ detail: {} }` to **discover the singleton instance** — the
drawer writes back `detail.instance = this`. See `util/discover.js`
for the helper.

### `EV_CONFIRM_DIALOG_REQUEST` — `crap:confirm-dialog-request`

Ask the user a yes/no question.

```js
document.dispatchEvent(new CustomEvent(EV_CONFIRM_DIALOG_REQUEST, {
  detail: {
    message: 'Discard changes?',
    onConfirm: () => navigateAway(),
    onCancel: () => null,
  },
}));
```

### `EV_DELETE_DIALOG_REQUEST` — `crap:delete-dialog-request`

Open the delete-confirmation dialog. The dialog handles soft-delete /
hard-delete / reference-counting automatically.

### `EV_CREATE_PANEL_REQUEST` — `crap:create-panel-request`

Open the inline create-panel drawer (used by relationship pickers
that allow creating the related document inline).

## Change notifications

### `EV_CHANGE` — `crap:change`

Bubbling event fired by **form-shaped components** when their value
changes. Listened to by the surrounding `<crap-dirty-form>` to mark
the form as having unsaved changes.

Emitting components: `<crap-tags>`, `<crap-code>`, `<crap-richtext>`,
`<crap-relationship-search>`, `<crap-uploads>`, `<crap-focal-point>`,
`<crap-conditions>`.

Plain `Event` — no `detail`. The current value is on the host
element (read via the standard form-element APIs or the component's
`.value` property).

```js
// Listen on a parent for any change in nested form-shaped components.
formEl.addEventListener('crap:change', (e) => {
  const host = e.target;
  console.log('changed:', host.tagName, host.value);
});
```

## Local notifications

These events stay **within a composite component tree** — parent and
child are colocated, the event doesn't reach `document`.

### `EV_PICK` — `crap:pick`

Emitted by `<crap-relationship-search>`'s dropdown when the user
selects an item. The parent search field listens to set its value.

### `EV_REQUEST_ADD_BLOCK` — `crap:request-add-block`

Emitted by `<crap-block-picker>` when a block type is chosen. Picked
up by the surrounding `<crap-array-field>` to insert a new block row.

## Adding behavior — capture-phase listener pattern

The recommended way to **add** behavior to a built-in component
(audit logging, analytics, validation) is to listen for its public
event in the capture phase:

```js
// <config_dir>/static/components/custom.js
document.addEventListener('crap:toast-request', (e) => {
  fetch('/api/audit', {
    method: 'POST',
    body: JSON.stringify({ message: e.detail.message }),
  });
  // No stopPropagation — upstream's <crap-toast> still gets the
  // event and shows the toast normally.
}, true /* capture phase */);
```

This pattern is **strictly additive**. You don't replace the toast
component, so any upstream improvements (bug fixes, new features)
flow through automatically. Compare:

- **Replace** the component: drop your own `static/components/toast.js`. You inherit no upstream improvements.
- **Add to** the component: capture-phase listener in `custom.js`.
  Upstream's behavior continues. You add yours alongside.

For most "I want to do X when toast Y happens" goals, the listener
pattern is cleaner.

## Custom events in your overlay

If you write your own `<crap-*>` components (registered via the
`custom.js` seam), follow the same naming convention:

- `crap:<name>-request` for singleton-request style
- `crap:<verb>` for bubbling changes
- Document the `detail` shape in a JSDoc typedef alongside the
  exported constant
- Export the constant from a stable module path

This keeps your custom events discoverable in the same way the
built-in ones are.

## Adding a new event to the built-in vocabulary

Adding to the built-in vocabulary requires an upstream PR — these
events are part of the public API. The contract:

1. **Add the named export** in
   [`static/components/events.js`](https://github.com/dkluhs/crap-cms/blob/main/static/components/events.js)
   with a JSDoc `@typedef` for the `detail` shape.
2. **Update this reference page** with the new entry.
3. **Use the constant**, not the string, in every dispatcher and
   listener — overlay authors will program against the constant.

Strings dispatched inline anywhere else (without an `EV_*` constant)
are **component-internal** and may change without warning.
