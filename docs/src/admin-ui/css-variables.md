# CSS Variables

The admin UI uses CSS custom properties for every design decision —
spacing, color, typography, sizes, shadows, transitions, and component-
specific knobs. Themes (`themes.css`) override these on
`html[data-theme="…"]`, so any component reading the variables
automatically participates in theming.

The full token set is defined in
[`static/styles.css`](https://github.com/dkluhs/crap-cms/blob/main/static/styles.css)
under `:root`. Below is the contract overlay authors and component
authors should code against.

## Base unit

```css
--base: 0.25rem;          /* 4px @ default font size */
```

Every spacing/size/control token is derived from `--base` with a small
multiplier so changing `--base` rescales the whole admin proportionally.

## Spacing (`--space-*`)

| Token         | Value          | Pixels (default) |
| ------------- | -------------- | ---------------- |
| `--space-2xs` | `base × 0.5`   | 2px              |
| `--space-xs`  | `base`         | 4px              |
| `--space-xs2` | `base × 1.5`   | 6px              |
| `--space-sm`  | `base × 2`     | 8px              |
| `--space-sm2` | `base × 2.5`   | 10px             |
| `--space-md`  | `base × 3`     | 12px             |
| `--space-lg`  | `base × 4`     | 16px             |
| `--space-xl`  | `base × 6`     | 24px             |
| `--space-2xl` | `base × 8`     | 32px             |

## Typography (`--text-*`)

| Token        | Value      | Pixels (default) |
| ------------ | ---------- | ---------------- |
| `--text-xs`  | `0.75rem`  | 12px             |
| `--text-sm`  | `0.8125rem`| 13px             |
| `--text-base`| `0.875rem` | 14px             |
| `--text-lg`  | `1rem`     | 16px             |
| `--text-xl`  | `1.125rem` | 18px             |
| `--text-2xl` | `1.375rem` | 22px             |

Font family is `Geist` (variable font, weights 100-900) with a
system-font fallback chain.

## Sizes

```css
--icon-xs:    calc(base × 3.5);     /* small status icons */
--icon-sm:    calc(base × 4);       /* button-inline icons */
--icon-md:    calc(base × 4.5);     /* default icon */
--icon-lg:    calc(base × 6);       /* section icons */
--icon-xl:    calc(base × 12);      /* hero/empty-state icons */

--control-sm: calc(base × 7);       /* small button height */
--control-md: calc(base × 8);
--control-lg: calc(base × 9);       /* default button/input height */

--button-height:    var(--control-lg);
--button-height-sm: var(--control-sm);
--input-height:     var(--control-lg);
```

## Radii

```css
--radius-sm:   4px;
--radius-md:   6px;
--radius-lg:   8px;
--radius-xl:  12px;
--radius-full: 9999px;
```

## Shadows

```css
--shadow-sm: 0 1px 2px rgba(0, 0, 0, 0.04);
--shadow-md: 0 2px 8px rgba(0, 0, 0, 0.06);
--shadow-lg: 0 4px 16px rgba(0, 0, 0, 0.08);
```

Themes override shadows for dark variants
(see `themes.css`).

## Transitions

```css
--transition-fast:   0.15s ease;
--transition-normal: 0.25s ease;
--transition-smooth: 0.3s cubic-bezier(0.215, 0.61, 0.355, 1);
```

## Color — semantic palette

The four "intents" each have a base, hover, active (where applicable),
and a low-opacity background (`-bg`) for tinted highlights.

| Family   | Base                | Hover                    | Active                    | BG (12% alpha)            |
| -------- | ------------------- | ------------------------ | ------------------------- | ------------------------- |
| Primary  | `--color-primary`   | `--color-primary-hover`  | `--color-primary-active`  | `--color-primary-bg`      |
| Danger   | `--color-danger`    | `--color-danger-hover`   | `--color-danger-active`   | `--color-danger-bg`       |
| Success  | `--color-success`   | —                        | —                         | `--color-success-bg`      |
| Warning  | `--color-warning`   | —                        | —                         | `--color-warning-bg`      |

Plus a separate semantic accent layer that points at primary by default
but lets themes redirect (`--accent-primary`, `--accent-primary-bg`).

## Text colors

```css
--text-primary:    /* default body text */
--text-secondary:  /* dimmer, e.g. card meta */
--text-tertiary:   /* dimmest, e.g. placeholders, help text */
--text-on-primary: /* foreground on primary-coloured surfaces */
```

## Surfaces & borders

```css
--bg-body:           /* page background */
--bg-surface:        /* card / input background */
--bg-elevated:       /* modals, panels, header */
--bg-hover:          /* generic hover wash */

--border-color:       /* default borders, separators */
--border-color-hover: /* slightly darker for inputs/buttons */

--surface-primary, --surface-secondary, --surface-hover  /* semantic aliases */
--border-default, --border-primary                       /* semantic aliases */
```

## Header & sidebar

```css
--header-height:        calc(base × 10);    /* sticky-header height */
--header-bg, --header-border

--sidebar-width:        calc(base × 52);
--sidebar-bg
--sidebar-active-bg
--sidebar-active-text
```

## Inputs

```css
--input-bg, --input-border, --input-height
--padding-with-icon:   calc(base × 10);   /* room for trailing icon */
--select-arrow:        <svg> data URL     /* themed dropdown chevron */
```

## Layout maxima

```css
--max-width-form:       calc(base × 100);
--dropdown-max-height:  calc(base × 60);
--preview-max-width:    calc(base × 50);
--preview-max-width-lg: calc(base × 75);
```

## Code-editor syntax palette

`<crap-code>` consumes a `--code-*` palette so syntax highlighting
follows the active theme. Light defaults live in `styles.css`; each
dark theme in `themes.css` redefines the full set.

```css
--code-keyword     /* control flow, declarations */
--code-string      /* string literals */
--code-number      /* numeric literals */
--code-comment     /* comments */
--code-atom        /* booleans, null, etc. */
--code-property    /* object property keys */
--code-function    /* function names */
--code-definition  /* declaration target name */
--code-type        /* type names */
--code-operator    /* +, -, etc. */
--code-regexp      /* regex literals */
--code-meta        /* preprocessor / shebangs */
--code-tag         /* HTML/XML tag names */
--code-attribute   /* HTML/XML attribute names */
--code-heading     /* Markdown heading */
--code-link        /* Markdown link / hyperlink */
```

## How themes override

`themes.css` redefines the relevant palette + surface + text tokens
under `html[data-theme="<name>"]`. The `<crap-theme-picker>` component
sets that attribute and persists the choice in `localStorage` under
the `crap-theme` key. A small inline FOUC-prevention script in
`layout/base.hbs` reads the same key on first paint to avoid a
light-then-dark flash on page load.

To add a custom theme, override `static/themes.css` (or append a new
selector block) defining at minimum:

- The full color palette (primary/danger/success/warning + bg variants)
- `--text-*` foreground colors
- `--bg-*` surface colors
- `--border-color` / `--border-color-hover`
- The `--code-*` palette
- `--input-bg` / `--input-border` / `--select-arrow`

Then wire the new theme into `<crap-theme-picker>` by adding a
`<button data-theme-value="…">` to its dropdown markup in
`layout/header.hbs` (or your override).
