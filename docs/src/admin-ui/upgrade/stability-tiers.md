# Stability tiers

Every public-surface module in the admin UI carries a `@stability`
JSDoc tag in its module-level comment:

```js
/**
 * Toast notifications — `<crap-toast>`.
 *
 * @module toast
 * @stability stable
 */
```

The tier tells you what to expect when you depend on that module's
public API across upgrades. **Three tiers**, plus a default for
files without a tag.

## Tier definitions

### `@stability stable`

The module is part of the **documented public API**. Its exports —
host attributes for components, named events, slots, partials,
config fields, CSS tokens — are covered by the deprecation policy.
Breaking changes get a major version bump and a deprecation cycle.

What's stable about a stable module:
- The tag name (for components)
- Documented attributes / parameters
- Documented events with their `detail` shapes
- Named slots and their contexts (where declared via `{{slot "name"}}`)
- CSS custom properties exported as theme tokens
- CSS `::part(...)` selectors **where exposed** via `part="..."`
  attributes inside the component's Shadow DOM

> **Note**: as of this release, **no built-in components expose
> `part="..."` attributes**, so `::part` selectors don't currently
> work as a customization path. They're listed here because they're
> part of the contract *if* a component declares them in a future
> release. For today's CSS-level customization, use theme tokens.

What's **not** part of the contract even on a stable module:
- Internal HTML structure (Shadow DOM components)
- Class names on internal elements (`.foo__bar` inside the component)
- Function names of private (`_method`) methods
- Internal CSS that isn't a custom property or an exposed `::part`

Override at your own pace. If upstream changes anything you depend
on, you'll see it in `crap-cms templates status` (for file overlays)
or as a deprecation warning in your dev-mode console (for runtime
contract changes).

### `@stability experimental`

The module is **shipped but its API is still settling**. Breaking
changes can land in any minor release. Used for features that work
but haven't gone through the design review for a stable contract
yet.

Three components currently carry this tier (run a grep at HEAD to
see the live list — they shift as features mature):

```
$ find static/components -name "*.js" -exec grep -l "@stability experimental" {} \;
```

**If you depend on an experimental module**, lock to a specific
crap-cms version in your deployment and pin the override file too —
treat the upgrade as opt-in. Read the changelog before bumping.

### `@stability internal`

The module is **plumbing**. Not for override. Its contract changes
without warning. This includes:

- `_internal/` modules (`h.js`, `css.js`, `i18n.js`, `util/*`)
- Sub-modules of public components (`richtext/`, `list-settings/`)
- The `index.js` entry point itself
- `groups.js`, `picker-base.js`

Listed in [components reference](../reference/components.md) for
discoverability, but **not** part of the override surface. If you
override an `internal` module, expect breakage on the next upgrade.

The leading underscore on `_internal/` is a visual marker for the
filesystem convention (Hugo's `_default/`, Next.js's `_folder` —
underscore = framework-reserved).

### Untagged

A handful of modules have no `@stability` tag yet. Treat them as
**`internal` until proven otherwise** — they haven't been formally
classified, and the absence of a tag means the maintainers haven't
committed to a contract. Open an issue if you have a use case for
overriding one; it might warrant promotion to `stable`.

## Tier counts at HEAD

```
$ find static/components -name "*.js" -exec grep -l "@stability stable" {} \; | wc -l
32
$ find static/components -name "*.js" -exec grep -l "@stability experimental" {} \; | wc -l
3
$ find static/components -name "*.js" -exec grep -l "@stability internal" {} \; | wc -l
20
```

Run these yourself to see the current split — the numbers shift as
the inventory matures (an experimental module promoted to stable,
internal helpers extracted from a stable module, etc.).

## Why tiers matter for overlays

The override surface is *every file* in `templates/` and `static/` —
but the **stability contract** only covers public API on stable
modules. When you override a stable component (drop a replacement
`toast.js`), you're forking a documented API; upstream commits to
not break your override silently.

When you override an internal module (drop a replacement `h.js`),
you're forking plumbing — upstream may rename, restructure, or
delete it without warning. `crap-cms templates status` will tell you
when it happens, but your repair work is on you.

In practice: **the public components you'd reasonably want to
customize are all `stable`**. The `internal` modules are the
plumbing you'd never touch unless you were explicitly hacking on
the framework.

## Promotion / demotion

Modules can move tiers between releases:

- **Experimental → stable** — when the API has settled and we're
  willing to commit to it. Announced in the release notes.
- **Stable → deprecated → removed** — when we want to retire
  something. The deprecation goes through at least one minor cycle
  with a warning before the next major version drops it.
- **Internal → stable** — rare. Happens when an internal helper
  ends up being so heavily depended on that we decide to promote
  it. The Rust `pub(crate)` → `pub` analogue.

Tier transitions are always announced in the changelog under a
**"Public surface"** heading.

## How to lint your overlay against tiers

The drift-detection toolkit doesn't currently distinguish overrides
by tier — `crap-cms templates status` reports drift uniformly. A
future `crap-cms doctor` command will flag overrides of
`experimental` or `internal` modules with a stronger warning, since
those are the ones most likely to break on upgrade.

In the meantime, the convention is:

- Override **stable** modules freely.
- Override **experimental** modules only with a version pin in your
  deployment.
- Don't override **internal** modules unless you know exactly what
  you're doing.
