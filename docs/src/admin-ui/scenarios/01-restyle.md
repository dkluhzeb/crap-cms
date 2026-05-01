# Scenario 1: Corporate restyle

**Goal**: replace the default admin colors, fonts, and spacing with
your brand.

**Difficulty**: low. ~15 minutes from scratch to a working theme.

**You'll touch**: CSS only. No templates, no Rust, no Lua.

## Approach

Crap CMS exposes its entire visual system as **CSS custom
properties** in
[`static/styles/tokens.css`](https://github.com/dkluhs/crap-cms/blob/main/static/styles/tokens.css).
Themes are CSS files that override a subset of these tokens on
`html[data-theme="..."]` selectors — every component reading the
variables automatically participates in theming.

You'll create a theme file with your brand tokens, register it in
the imports chain, and (optionally) add a picker entry so users can
switch into it.

## Step 1 — drop your theme file

Create `<config_dir>/static/styles/themes/themes-acme.css`:

```css
html[data-theme="acme"] {
  color-scheme: light;

  /* Brand colors */
  --color-primary: #ff5500;
  --color-primary-hover: #ff7733;
  --color-primary-active: #d94800;
  --color-primary-bg: rgba(255, 85, 0, 0.08);

  /* Optional — keep danger/success/warning untouched, or restyle */
  --color-danger: #d62828;

  /* Optional — adjust shadows for your brand vibe */
  --shadow-md: 0 4px 12px rgba(255, 85, 0, 0.10);

  /* Optional — your font stack */
  --font-family: "Inter", system-ui, -apple-system, sans-serif;
}
```

Only override the tokens you actually want to change — every other
value falls back to the default in `tokens.css`.

See [CSS variables reference](../reference/css-variables.md) for the
full token catalogue.

## Step 2 — register the theme in the import chain

Override the entry stylesheet to include your theme. Drop
`<config_dir>/static/styles/main.css` (extracted from upstream, then
add your theme at the end):

```
$ crap-cms templates extract styles/main.css
```

Edit the extracted file and add your theme as the **last** import
(themes load after `default.css` so they can override its tokens):

```css
/* <config_dir>/static/styles/main.css */
@import url("/static/styles/tokens.css");
@import url("/static/styles/base/normalize.css");
@import url("/static/styles/base/fonts.css");
@import url("/static/styles/base/reset.css");
@import url("/static/styles/layout/layout.css");
@import url("/static/styles/layout/edit-sidebar.css");
@import url("/static/styles/parts/cards.css");
@import url("/static/styles/parts/forms.css");
@import url("/static/styles/parts/buttons.css");
@import url("/static/styles/parts/lists.css");
@import url("/static/styles/parts/tables.css");
@import url("/static/styles/parts/breadcrumb.css");
@import url("/static/styles/parts/badges.css");
@import url("/static/styles/parts/pagination.css");
@import url("/static/styles/themes/default.css");
@import url("/static/styles/themes/themes-acme.css");  /* ← your theme */
```

## Step 3 — activate the theme

The admin's theme picker reads from `localStorage`'s `crap-theme`
key. Set it once in the browser console:

```js
localStorage.setItem('crap-theme', 'acme');
location.reload();
```

Or reach in via the admin's `theme` enhancer:

```js
window.crap.theme.set('acme');
```

The `<html data-theme="acme">` attribute toggles, your theme's
custom properties take effect, and every component re-renders with
your colors.

## Step 4 (optional) — set as the default

To make `acme` the default for new visitors (instead of `light`),
override the theme bootstrap script that runs in `<head>` before
paint. Drop `<config_dir>/templates/layout/base.hbs` (extracted from
upstream) and change the bootstrap line:

```hbs
<script nonce="{{crap.csp_nonce}}">
  (function(){
    var t = localStorage.getItem('crap-theme') || 'acme';
    document.documentElement.setAttribute('data-theme', t);
  })();
</script>
```

## Step 5 (optional) — also rebrand the wordmark + logo

You've covered colors and fonts. To finish the rebrand, see:

- [Site name](../index.md) — `[admin] site_name = "Acme CMS"` in
  `crap.toml`.
- [Logo partial](../guides/partials.md#worked-example--swap-the-brand-logo)
  — drop your SVG at `templates/partials/logo.hbs`.
- [Theme color meta tag](../guides/partials.md) — drop a custom
  `templates/partials/meta-tags.hbs` with your `theme-color`.

## Verifying

```
$ crap-cms templates status
```

should show your override files as `· user-original (no upstream
counterpart)` — they're new files, not extracted-and-edited copies
of upstream — and any extracted files (like `main.css`) as
`✓ current` while you're on the same release.

After upgrading crap-cms, run `templates status` again — your theme
file stays `user-original`, but `main.css` may show `behind` if
upstream added a new `@import`. Use `templates diff styles/main.css`
to see what to add.
